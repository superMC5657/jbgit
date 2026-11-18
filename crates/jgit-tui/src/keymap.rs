//! Dynamic keymap parser, sequence matcher, and action dispatcher.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jgit_core::{BranchNodeKind, PaneId, WorkspaceTab};

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuEntryKind, ContextMenuItem, PopupKind};
use crate::config::keymap_config::{KeyBindingConfig, KeymapConfig};

pub const KEY_CHORD_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

fn parse_named_key(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "backspace" => Some(KeyCode::Backspace),
        "del" | "delete" => Some(KeyCode::Delete),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "pgdn" => Some(KeyCode::PageDown),
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            f[1..].parse::<u8>().ok().map(KeyCode::F)
        }
        _ => None,
    }
}

impl KeyStroke {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Bracketed notation, e.g. "<C-d>", "<A-1>", "<Space>", "<C-Enter>", "<F2>"
        if s.starts_with('<') && s.ends_with('>') && s.len() >= 3 {
            let inner = &s[1..s.len() - 1];
            let parts: Vec<&str> = inner.split('-').collect();
            if parts.is_empty() {
                return None;
            }

            let mut ctrl = false;
            let mut alt = false;
            let mut shift = false;

            for &m in &parts[..parts.len() - 1] {
                match m.to_ascii_lowercase().as_str() {
                    "c" | "ctrl" | "control" => ctrl = true,
                    "a" | "alt" => alt = true,
                    "s" | "shift" => shift = true,
                    _ => {}
                }
            }

            let key_part = parts[parts.len() - 1];
            let code = parse_named_key(key_part).or_else(|| {
                let mut chars = key_part.chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(KeyCode::Char(first))
            })?;

            Some(KeyStroke {
                code,
                ctrl,
                alt,
                shift,
            })
        } else {
            // Non-bracketed representation: "q", "G", "j", "k", "r", " ", etc.
            let mut chars = s.chars();
            let first = chars.next()?;
            if chars.next().is_none() {
                let shift = first.is_ascii_uppercase();
                Some(KeyStroke {
                    code: KeyCode::Char(first),
                    ctrl: false,
                    alt: false,
                    shift,
                })
            } else {
                let code = parse_named_key(s)?;
                Some(KeyStroke {
                    code,
                    ctrl: false,
                    alt: false,
                    shift: false,
                })
            }
        }
    }

    pub fn matches(&self, event: &KeyEvent) -> bool {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);

        if self.ctrl != ctrl || self.alt != alt {
            return false;
        }

        match (self.code, event.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                if self.ctrl || self.alt {
                    a.eq_ignore_ascii_case(&b)
                } else if a.is_ascii_uppercase() {
                    a == b || (a.to_ascii_lowercase() == b && shift)
                } else {
                    a == b && !shift
                }
            }
            (a, b) => a == b && self.shift == shift,
        }
    }

    pub fn to_display_string(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift && !matches!(self.code, KeyCode::Char(c) if c.is_ascii_uppercase()) {
            s.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) => s.push(c),
            KeyCode::Enter => s.push_str("Enter"),
            KeyCode::Esc => s.push_str("Esc"),
            KeyCode::Backspace => s.push_str("Backspace"),
            KeyCode::Delete => s.push_str("Del"),
            KeyCode::Up => s.push_str("Up"),
            KeyCode::Down => s.push_str("Down"),
            KeyCode::Left => s.push_str("Left"),
            KeyCode::Right => s.push_str("Right"),
            KeyCode::Home => s.push_str("Home"),
            KeyCode::End => s.push_str("End"),
            KeyCode::PageUp => s.push_str("PgUp"),
            KeyCode::PageDown => s.push_str("PgDn"),
            KeyCode::Tab => s.push_str("Tab"),
            KeyCode::BackTab => s.push_str("Shift+Tab"),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            _ => s.push('?'),
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySequence {
    pub strokes: Vec<KeyStroke>,
}

impl KeySequence {
    pub fn parse(keys: &[String]) -> Option<Self> {
        if keys.is_empty() {
            return None;
        }
        let mut strokes = Vec::with_capacity(keys.len());
        for k in keys {
            strokes.push(KeyStroke::parse(k)?);
        }
        Some(Self { strokes })
    }

    pub fn matches_exact(&self, events: &[KeyEvent]) -> bool {
        if self.strokes.len() != events.len() {
            return false;
        }
        self.strokes
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn matches_prefix(&self, events: &[KeyEvent]) -> bool {
        if events.is_empty() || events.len() >= self.strokes.len() {
            return false;
        }
        self.strokes[..events.len()]
            .iter()
            .zip(events.iter())
            .all(|(stroke, ev)| stroke.matches(ev))
    }

    pub fn to_display_string(&self) -> String {
        self.strokes
            .iter()
            .map(|s| s.to_display_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub enum MatchResult<'a> {
    Exact(&'a KeyBindingConfig),
    Prefix,
    None,
}

pub fn match_bindings<'a>(
    bindings: &'a [KeyBindingConfig],
    events: &[KeyEvent],
) -> MatchResult<'a> {
    let mut prefix_found = false;

    for binding in bindings {
        if let Some(seq) = KeySequence::parse(&binding.on) {
            if seq.matches_exact(events) {
                return MatchResult::Exact(binding);
            }
            if seq.matches_prefix(events) {
                prefix_found = true;
            }
        }
    }

    if prefix_found {
        MatchResult::Prefix
    } else {
        MatchResult::None
    }
}

fn find_action_by_mnemonic(items: Vec<ContextMenuItem>, mnemonic: char) -> Option<Vec<Action>> {
    let target = mnemonic.to_ascii_lowercase();
    for item in items {
        if item.disabled_reason.is_some() {
            continue;
        }
        let matches = item.mnemonic.is_some_and(|m| m.to_ascii_lowercase() == target);
        match item.kind {
            ContextMenuEntryKind::Action(ContextMenuAction::Run(actions)) if matches => {
                return Some(actions);
            }
            ContextMenuEntryKind::Submenu(sub) if matches => {
                if let Some(first) = sub.into_iter().find_map(|s| match s.kind {
                    ContextMenuEntryKind::Action(ContextMenuAction::Run(a))
                        if s.disabled_reason.is_none() =>
                    {
                        Some(a)
                    }
                    _ => None,
                }) {
                    return Some(first);
                }
            }
            ContextMenuEntryKind::Submenu(sub) => {
                if let Some(found) = find_action_by_mnemonic(sub, mnemonic) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

fn branch_menu_action_by_mnemonic(state: &AppState, mnemonic: char) -> Option<Vec<Action>> {
    let node = state.branches.selected_node()?;
    let BranchNodeKind::Leaf(kind) = node.kind else {
        return None;
    };
    let ref_name = node.ref_name.as_deref()?;
    let items = crate::menus::branch_menu_items(state, ref_name, kind);
    find_action_by_mnemonic(items, mnemonic)
}

pub fn execute_action(run: &str, state: &mut AppState) -> Option<Vec<Action>> {
    match run {
        // ── Global ──
        "quit" => Some(vec![Action::Quit]),
        "help" => Some(vec![Action::OpenPopup {
            kind: PopupKind::ShortcutsHelp,
            initial_query: String::new(),
        }]),
        "refresh" => Some(vec![Action::RefreshAll]),
        "tab_log" => Some(vec![Action::SwitchTab(WorkspaceTab::Log)]),
        "tab_commit" => Some(vec![Action::SwitchTab(WorkspaceTab::Commit)]),
        "tab_branches" => Some(vec![
            Action::SwitchTab(WorkspaceTab::Log),
            Action::FocusPane(PaneId::LogBranchesTree),
        ]),
        "tab_stash" => Some(vec![Action::SwitchTab(WorkspaceTab::Stash)]),
        "toggle_branches_popup" => Some(vec![Action::OpenBranchesPopup {
            query: String::new(),
        }]),
        "palette" => {
            if state.active_tab == WorkspaceTab::Log {
                Some(vec![Action::OpenPopup {
                    kind: PopupKind::GoToRevision,
                    initial_query: String::new(),
                }])
            } else {
                Some(vec![Action::OpenPopup {
                    kind: PopupKind::PalettePreview,
                    initial_query: String::new(),
                }])
            }
        }
        "goto_revision" => Some(vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }]),

        // ── Log ──
        "cursor_down" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffScroll(1)]),
            PaneId::LogDetailsDiff if state.details.diff_preview || state.details.diff_zoomed => {
                Some(vec![Action::ScrollDiff(1)])
            }
            PaneId::StashDiffPreview => Some(vec![Action::StashDiffScroll(1)]),
            _ => Some(vec![Action::ScrollLog(1)]),
        },
        "cursor_up" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::ScrollBranches(-1)]),
            PaneId::CommitChangesTree => Some(vec![Action::ScrollCommitStage(-1)]),
            PaneId::CommitDiffPreview => Some(vec![Action::StageDiffScroll(-1)]),
            PaneId::LogDetailsDiff if state.details.diff_preview || state.details.diff_zoomed => {
                Some(vec![Action::ScrollDiff(-1)])
            }
            PaneId::StashDiffPreview => Some(vec![Action::StashDiffScroll(-1)]),
            _ => Some(vec![Action::ScrollLog(-1)]),
        },
        "compare_versions" => {
            let oids = state.selected_commits_oldest_first();
            if let [base, target] = oids.as_slice() {
                Some(vec![Action::CompareVersions {
                    base: *base,
                    target: *target,
                }])
            } else {
                Some(vec![Action::LogPage {
                    half: true,
                    down: true,
                }])
            }
        }
        "checkout" => {
            if state.focused_pane == PaneId::LogBranchesTree {
                branch_menu_action_by_mnemonic(state, 'c')
            } else {
                let items = crate::menus::log_commit_menu_items(state);
                find_action_by_mnemonic(items, 'c')
            }
        }
        "goto_top" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(0)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(0)]),
            _ => Some(vec![Action::LogEdge { bottom: false }]),
        },
        "goto_bottom" => match state.focused_pane {
            PaneId::LogBranchesTree => Some(vec![Action::SelectBranchRow(usize::MAX)]),
            PaneId::CommitChangesTree => Some(vec![Action::SelectCommitStageRow(usize::MAX)]),
            _ => Some(vec![Action::LogEdge { bottom: true }]),
        },
        "focus_filter" => Some(vec![Action::FocusFilterText]),
        "uncommit" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'u')
        }
        "reword" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'w')
        }
        "drop_commit" => {
            let items = crate::menus::log_commit_menu_items(state);
            find_action_by_mnemonic(items, 'x')
        }

        // ── Commit workbench ──
        "toggle_stage" => {
            if state.focused_pane == PaneId::CommitDiffPreview {
                let side = state.stage_workbench.current_diff_key.as_ref().map(|k| k.side);
                let is_amend_ref = side == Some(jgit_engine::StageDiffSide::AmendCommit);
                if !is_amend_ref {
                    if side == Some(jgit_engine::StageDiffSide::Staged) {
                        Some(vec![Action::StageDiffUnstageSelection])
                    } else {
                        Some(vec![Action::StageDiffStageSelection])
                    }
                } else {
                    Some(Vec::new())
                }
            } else {
                let row = state.stage_workbench.rows.get(state.commit_stage_selected_idx);
                match row {
                    Some(r) if r.group_kind().can_stage() => Some(vec![Action::StageActionSelected]),
                    Some(r) if r.group_kind().can_unstage() => Some(vec![Action::UnstageActionSelected]),
                    Some(r) if r.is_container() => {
                        Some(vec![Action::StageToggleRow(state.commit_stage_selected_idx)])
                    }
                    _ => Some(Vec::new()),
                }
            }
        }
        "commit" => Some(vec![Action::SubmitCommit { and_push: false }]),
        "toggle_amend" => Some(vec![Action::CommitToggleAmend]),
        "select_all" => Some(vec![Action::StageSelectAll]),
        "discard_changes" => Some(vec![Action::DiscardPromptSelected]),
        "toggle_group" => Some(vec![Action::StageToggleGroupByDir]),
        "edit_file" => Some(vec![Action::OpenExternalEditorFile(None)]),

        // ── Branch ──
        "toggle_favorite" => Some(vec![Action::BranchesToggleFavorite]),
        "rename_branch" => branch_menu_action_by_mnemonic(state, 'e'),
        "delete_branch" => branch_menu_action_by_mnemonic(state, 'd'),

        // ── Diff ──
        "toggle_side_by_side" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffToggleSideBySide]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffToggleSideBySide]),
            _ => Some(vec![Action::DiffToggleSideBySide]),
        },
        "cycle_diff_context" => match state.active_tab {
            WorkspaceTab::Commit => Some(vec![Action::StageDiffCycleContext]),
            WorkspaceTab::Stash => Some(vec![Action::StashDiffCycleContext]),
            _ => Some(vec![Action::DiffCycleContext]),
        },
        "cycle_diff_dock" => Some(vec![Action::DiffCycleDock]),
        "prev_file" => Some(vec![Action::DiffNextFile(-1)]),
        "next_file" => Some(vec![Action::DiffNextFile(1)]),
        "zoom_diff" => Some(vec![Action::ZoomDiff(!state.details.diff_zoomed)]),

        _ => None,
    }
}

pub fn check_scopes<'a>(
    events: &[KeyEvent],
    keymap: &'a KeymapConfig,
    active_tab: WorkspaceTab,
    focused_pane: PaneId,
    diff_focused: bool,
) -> MatchResult<'a> {
    if diff_focused {
        match match_bindings(&keymap.diff.key_bindings, events) {
            MatchResult::None => {}
            other => return other,
        }
    }

    // Pane / tab-specific bindings
    if active_tab == WorkspaceTab::Log {
        if focused_pane == PaneId::LogBranchesTree {
            match match_bindings(&keymap.branch.key_bindings, events) {
                MatchResult::None => {}
                other => return other,
            }
        }
        match match_bindings(&keymap.log.key_bindings, events) {
            MatchResult::None => {}
            other => return other,
        }
    } else if active_tab == WorkspaceTab::Commit {
        match match_bindings(&keymap.commit.key_bindings, events) {
            MatchResult::None => {}
            other => return other,
        }
    }

    // Global fallback
    match_bindings(&keymap.global.key_bindings, events)
}

fn is_diff_focused(state: &AppState) -> bool {
    match state.active_tab {
        WorkspaceTab::Log => {
            state.details.diff_zoomed
                || (state.focused_pane == PaneId::LogDetailsDiff && state.details.diff_preview)
        }
        WorkspaceTab::Commit => state.focused_pane == PaneId::CommitDiffPreview,
        WorkspaceTab::Stash => state.focused_pane == PaneId::StashDiffPreview,
        WorkspaceTab::Console => false,
    }
}

/// Routes an incoming key through the user-configured keymap rules.
/// Returns `Some(actions)` if matched or consumed (for multi-key prefix).
/// Returns `None` if no keymap rule matched, allowing fallback to default handlers.
pub fn route_dynamic_key(key: &KeyEvent, state: &mut AppState) -> Option<Vec<Action>> {
    // Check if the chord timeout has expired
    if let Some(t) = state.pending_key_at
        && t.elapsed() > KEY_CHORD_WINDOW
    {
        state.pending_keys.clear();
        state.pending_key_at = None;
    }

    state.pending_keys.push(*key);

    let active_tab = state.active_tab;
    let focused_pane = state.focused_pane;
    let diff_focused = is_diff_focused(state);

    match check_scopes(&state.pending_keys, &state.config.keymap, active_tab, focused_pane, diff_focused) {
        MatchResult::Exact(binding) => {
            let run = binding.run.clone();
            state.pending_keys.clear();
            state.pending_key_at = None;
            execute_action(&run, state)
        }
        MatchResult::Prefix => {
            state.pending_key_at = Some(Instant::now());
            Some(Vec::new()) // Consumed, waiting for the rest of sequence
        }
        MatchResult::None => {
            if state.pending_keys.len() > 1 {
                // Multi-key chord failed, try matching just this single latest key
                state.pending_keys = vec![*key];
                match check_scopes(
                    &state.pending_keys,
                    &state.config.keymap,
                    active_tab,
                    focused_pane,
                    diff_focused,
                ) {
                    MatchResult::Exact(binding) => {
                        let run = binding.run.clone();
                        state.pending_keys.clear();
                        state.pending_key_at = None;
                        execute_action(&run, state)
                    }
                    MatchResult::Prefix => {
                        state.pending_key_at = Some(Instant::now());
                        Some(Vec::new())
                    }
                    MatchResult::None => {
                        state.pending_keys.clear();
                        state.pending_key_at = None;
                        None
                    }
                }
            } else {
                state.pending_keys.clear();
                state.pending_key_at = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_match_keystrokes() {
        let k = KeyStroke::parse("<C-d>").unwrap();
        assert_eq!(k.code, KeyCode::Char('d'));
        assert!(k.ctrl);
        assert!(!k.alt);

        let ev_ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(k.matches(&ev_ctrl_d));

        let ev_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(!k.matches(&ev_d));

        let k_space = KeyStroke::parse("<Space>").unwrap();
        let ev_space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(k_space.matches(&ev_space));

        let k_f2 = KeyStroke::parse("<F2>").unwrap();
        let ev_f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert!(k_f2.matches(&ev_f2));

        let k_a1 = KeyStroke::parse("<A-1>").unwrap();
        let ev_a1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT);
        assert!(k_a1.matches(&ev_a1));
    }

    #[test]
    fn sequence_exact_and_prefix_matching() {
        let seq = KeySequence::parse(&["g".to_string(), "g".to_string()]).unwrap();
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);

        assert!(seq.matches_prefix(&[ev_g]));
        assert!(seq.matches_exact(&[ev_g, ev_g]));

        let ev_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(!seq.matches_prefix(&[ev_j]));
        assert!(!seq.matches_exact(&[ev_g, ev_j]));
    }

    #[test]
    fn dynamic_keymap_routing_and_remapping() {
        use std::path::PathBuf;
        use crate::config::AppConfig;

        let config = AppConfig::default();
        let mut state = AppState::new_with_config(PathBuf::from("."), None, config);

        // 1. Default routing: "r" refreshes
        let ev_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let actions = route_dynamic_key(&ev_r, &mut state);
        assert!(matches!(actions.as_deref(), Some([Action::RefreshAll])));

        // 2. Default routing: multi-key sequence "g" then "g"
        let ev_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let first = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(first.as_deref(), Some([]))); // prefix consumed
        let second = route_dynamic_key(&ev_g, &mut state);
        assert!(matches!(
            second.as_deref(),
            Some([Action::LogEdge { bottom: false }])
        ));

        // 3. Diff scope routing
        state.active_tab = WorkspaceTab::Log;
        state.focused_pane = PaneId::LogDetailsDiff;
        state.details.diff_preview = true;
        let ev_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        let actions_s = route_dynamic_key(&ev_s, &mut state);
        assert!(matches!(
            actions_s.as_deref(),
            Some([Action::DiffToggleSideBySide])
        ));

        // 4. Custom keymap: remap refresh to F5 via overlay
        let mut custom_config = AppConfig::default();
        let ov: crate::config::KeymapConfigOverlay = toml::from_str(
            r#"
[global]
key_bindings = [{ on = ["<F5>"], run = "refresh" }]
"#,
        )
        .unwrap();
        custom_config.keymap.merge_overlay(&ov);
        let mut state_custom = AppState::new_with_config(PathBuf::from("."), None, custom_config);

        // Pressing F5 now refreshes
        let ev_f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        let actions_f5 = route_dynamic_key(&ev_f5, &mut state_custom);
        assert!(matches!(actions_f5.as_deref(), Some([Action::RefreshAll])));

        // Pressing r does NOT refresh
        let actions_r = route_dynamic_key(&ev_r, &mut state_custom);
        assert!(actions_r.is_none());

        // Existing global bindings like 'q' are preserved
        let ev_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let actions_q = route_dynamic_key(&ev_q, &mut state_custom);
        assert!(matches!(actions_q.as_deref(), Some([Action::Quit])));
    }
}
