use super::*;

fn paginated_snapshot(count: u8, loaded_limit: usize, has_more_commits: bool) -> RepoSnapshot {
    let commits: Vec<CommitSummary> = (1..=count)
        .rev()
        .map(|n| CommitSummary {
            oid: oid(n),
            parents: if n > 1 { vec![oid(n - 1)] } else { vec![] },
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: 1_700_000_000 + i64::from(n) * 60,
            author_timestamp: 1_700_000_000 + i64::from(n) * 60,
            subject: format!("commit {n:02}"),
            body: String::new(),
        })
        .collect();
    let mut main = local("main", count);
    main.is_head = true;
    RepoSnapshot {
        repo_name: "big-repo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(count)),
        refs: vec![main],
        commits: commits.into(),
        loaded_limit,
        has_more_commits,
        ..Default::default()
    }
}

#[test]
fn load_more_virtual_row_renders_clicks_enters_and_wires_command_catalog() {
    let mut ts = TestSession::with_snapshot(paginated_snapshot(5, 5, true));
    ts.state_mut().commit_page_size = 5;
    let buf = ts.render(120, 20);
    let text = screen_text(&buf);
    assert!(
        text.contains("5+ commits"),
        "expected '5+ commits' in Log header, got:\n{text}"
    );
    assert!(
        text.contains("Load More (+5 commits)"),
        "expected virtual Load More row, got:\n{text}"
    );

    // HitTarget::LogLoadMore is registered and clickable.
    let load_more_rect = ts.region_of(|t| matches!(t, HitTarget::LogLoadMore));

    // CommandCatalog applicability & execution.
    assert!(crate::keymap::catalog::is_action_applicable(
        &crate::keymap::KeyAction::LogLoadMore,
        ts.state()
    ));
    let catalog_actions =
        crate::keymap::catalog::execute_action(&crate::keymap::KeyAction::LogLoadMore, ts.state())
            .expect("LogLoadMore produces actions");
    assert!(
        matches!(catalog_actions.as_slice(), [Action::LoadMoreCommits]),
        "unexpected catalog actions: {catalog_actions:?}"
    );

    // Mouse click on virtual tail row dispatches TaskRequest::LoadMoreCommits.
    ts.take_requests();
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        load_more_rect.x + 2,
        load_more_rect.y,
    );
    let recorded = ts.take_requests();
    assert!(
        recorded
            .iter()
            .any(|(req, _)| matches!(req, TaskRequest::LoadMoreCommits)),
        "clicking LogLoadMore must dispatch TaskRequest::LoadMoreCommits; got {recorded:?}"
    );
    assert!(ts.state().loading_more_commits);
    assert!(!crate::keymap::catalog::is_action_applicable(
        &crate::keymap::KeyAction::LogLoadMore,
        ts.state()
    ));

    // While loading, virtual row shows loading label and ignores duplicate clicks.
    let loading_buf = ts.render(120, 20);
    assert!(screen_text(&loading_buf).contains("Loading more commits"));
    ts.send_mouse(
        MouseEventKind::Down(MouseButton::Left),
        load_more_rect.x + 2,
        load_more_rect.y,
    );
    assert!(
        ts.take_requests().is_empty(),
        "in-flight LoadMoreCommits must ignore duplicate triggers"
    );

    // Deliver the next page (10 commits, still has_more = true) on the current RepoSnapshot epoch.
    let latest_epoch = ts
        .engine
        .latest_epoch(jbgit_core::RequestSlotId::RepoSnapshot);
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(paginated_snapshot(10, 10, true)),
        epoch: latest_epoch,
    });
    assert!(!ts.state().loading_more_commits);
    assert_eq!(ts.state().log.graph.visible_count(), 10);

    // Move cursor to bottom (`G`) onto the virtual Load More row (index 10) and trigger via Enter.
    ts.dispatch(Action::LogEdge { bottom: true });
    assert!(ts.state().log.graph.is_cursor_on_load_more());
    assert!(
        ts.state()
            .log
            .graph
            .selected_oids(ts.state().repo_snapshot.as_ref())
            .is_empty()
    );

    ts.take_requests();
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let recorded_enter = ts.take_requests();
    assert!(
        recorded_enter
            .iter()
            .any(|(req, _)| matches!(req, TaskRequest::LoadMoreCommits)),
        "Enter on virtual Load More row must dispatch LoadMoreCommits; got {recorded_enter:?}"
    );

    // Verify zh-CN rendering of the virtual tail row.
    let mut zh_ts = TestSession::with_snapshot(paginated_snapshot(5, 50_000, true));
    zh_ts.state_mut().i18n = Arc::new(I18n::load("zh-CN", None));
    zh_ts.state_mut().commit_page_size = 50_000;
    let zh_buf = zh_ts.render(120, 20);
    let zh_text = screen_text(&zh_buf);
    let zh_compact: String = zh_text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        zh_compact.contains("加载更多(+50,000条提交)"),
        "expected zh-CN Load More label, got:\n{zh_text}"
    );
}

#[test]
fn load_more_preserves_scroll_offset_and_selected_oid_losslessly() {
    let mut ts = TestSession::with_snapshot(paginated_snapshot(10, 10, true));
    ts.state_mut().commit_page_size = 10;
    ts.state_mut().log.graph.last_viewport_h = 5;

    // Select row 7 (oid(3)) so scroll_offset advances to 3 (showing rows 3..8).
    ts.dispatch(Action::JumpToLogRow {
        row: 7,
        center: false,
    });
    let selected_before = ts
        .state()
        .log
        .graph
        .selected_oid(ts.state().repo_snapshot.as_ref())
        .expect("selected commit");
    let scroll_before = ts.state().log.graph.scroll_offset;
    assert_eq!(ts.state().log.graph.selected_idx, 7);
    assert_eq!(scroll_before, 3);

    // Trigger LoadMore and deliver 20-commit snapshot whose top 10 commits are identical.
    ts.dispatch(Action::LoadMoreCommits);
    let epoch = ts
        .engine
        .latest_epoch(jbgit_core::RequestSlotId::RepoSnapshot);

    // Build 20-commit snapshot preserving the exact top 10 OIDs (oid(10)..=oid(1))
    // followed by 10 older commits (oid(20)..=oid(11)).
    let mut commits = Vec::new();
    for n in (1..=10u8).rev() {
        let parent = if n > 1 { oid(n - 1) } else { oid(20) };
        commits.push(CommitSummary {
            oid: oid(n),
            parents: vec![parent],
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: 1_700_000_000 + i64::from(n + 10) * 60,
            author_timestamp: 1_700_000_000 + i64::from(n + 10) * 60,
            subject: format!("commit {n:02}"),
            body: String::new(),
        });
    }
    for n in (11..=20u8).rev() {
        commits.push(CommitSummary {
            oid: oid(n),
            parents: if n > 11 { vec![oid(n - 1)] } else { vec![] },
            author_name: "Alice".into(),
            author_email: "alice@x.io".into(),
            timestamp: 1_700_000_000 + i64::from(n - 10) * 60,
            author_timestamp: 1_700_000_000 + i64::from(n - 10) * 60,
            subject: format!("older {n:02}"),
            body: String::new(),
        });
    }
    let mut main = local("main", 10);
    main.is_head = true;
    let expanded = RepoSnapshot {
        repo_name: "big-repo".into(),
        head_branch: Some("main".into()),
        head_oid: Some(oid(10)),
        refs: vec![main],
        commits: commits.into(),
        loaded_limit: 20,
        has_more_commits: false,
        ..Default::default()
    };

    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(expanded),
        epoch,
    });

    assert_eq!(ts.state().log.graph.visible_count(), 20);
    assert!(!ts.state().log.graph.has_load_more_row());
    assert_eq!(ts.state().log.graph.selected_idx, 7);
    assert_eq!(
        ts.state()
            .log
            .graph
            .selected_oid(ts.state().repo_snapshot.as_ref()),
        Some(selected_before),
        "selected commit OID must be preserved across LoadMore"
    );
    assert_eq!(
        ts.state().log.graph.scroll_offset,
        scroll_before,
        "viewport scroll_offset must be preserved losslessly across LoadMore"
    );
}

#[test]
fn enter_on_virtual_load_more_lands_on_first_new_commit_without_jumping_to_top() {
    let mut ts = TestSession::with_snapshot(paginated_snapshot(10, 10, true));
    ts.state_mut().commit_page_size = 10;
    ts.state_mut().log.graph.last_viewport_h = 5;

    // Jump to bottom (`G`): cursor lands on virtual row 10, scroll_offset = 6 (rows 6..=10).
    ts.dispatch(Action::LogEdge { bottom: true });
    assert!(ts.state().log.graph.is_cursor_on_load_more());
    assert_eq!(ts.state().log.graph.selected_idx, 10);
    assert_eq!(ts.state().log.graph.scroll_offset, 6);

    // Press Enter to load more commits.
    ts.send_key(KeyCode::Enter, KeyModifiers::NONE);
    let epoch = ts
        .engine
        .latest_epoch(jbgit_core::RequestSlotId::RepoSnapshot);
    ts.deliver_engine_event(EngineEvent::RepoSnapshotLoaded {
        snapshot: Arc::new(paginated_snapshot(20, 20, false)),
        epoch,
    });

    assert_eq!(ts.state().log.graph.visible_count(), 20);
    assert_eq!(
        ts.state().log.graph.selected_idx,
        10,
        "cursor should advance to the first newly loaded commit rather than resetting to row 0"
    );
    assert_eq!(
        ts.state().log.graph.scroll_offset,
        6,
        "scroll_offset should remain anchored where the user pressed Enter"
    );
}

#[test]
fn log_load_more_key_action_has_keymap_and_i18n_parity() {
    use crate::config::{KeymapConfig, KeymapPreset};
    use crate::keymap::KeyAction;
    assert_eq!(KeyAction::parse("log_load_more"), KeyAction::LogLoadMore);
    assert_eq!(KeyAction::LogLoadMore.as_str(), "log_load_more");
    assert_eq!(
        KeyAction::LogLoadMore.footer_pill_key(),
        Some("view.footer.pill.log_load_more")
    );
    for locale in ["en-US", "zh-CN"] {
        let i18n = I18n::load(locale, None);
        assert_ne!(
            i18n.t("help.action.log_load_more"),
            "help.action.log_load_more"
        );
        assert_ne!(
            i18n.t("view.footer.pill.log_load_more"),
            "view.footer.pill.log_load_more"
        );
        assert_ne!(i18n.t("menu.log.load_more"), "menu.log.load_more");
    }
    for preset in [
        KeymapPreset::Default,
        KeymapPreset::Jetbrains,
        KeymapPreset::Vim,
    ] {
        let km = KeymapConfig::for_preset(preset.clone());
        let mut ts = TestSession::with_snapshot(paginated_snapshot(5, 5, true));
        ts.state_mut().config.keymap = km;
        assert!(
            crate::keymap::catalog::is_action_applicable(&KeyAction::LogLoadMore, ts.state()),
            "LogLoadMore should be applicable on preset {preset:?}"
        );
    }
}
