//! Typed keymap scope resolution and sequence matching (command specs live in [`super::catalog`]).

use crossterm::event::{KeyCode, KeyEvent};
use jbgit_core::{PaneId, WorkspaceTab};
use jbgit_engine::StageDiffSide;

use crate::app::{Action, AppState};
use crate::config::keymap_config::{KeyBindingConfig, KeymapScope};

pub use super::catalog::{execute_action, is_action_applicable};
use super::{KEY_CHORD_WINDOW, KeyAction, KeySequence, MatchResult, match_bindings_filtered};

pub const DEFAULT_CONSOLE_PROBE_CHAR: char = 'p';

pub fn console_status_probe_key(keymap: &crate::config::KeymapConfig) -> String {
    keymap
        .primary_key_for(&[KeymapScope::Global], &KeyAction::GitStatusProbe)
        .unwrap_or_else(|| DEFAULT_CONSOLE_PROBE_CHAR.to_string())
}

pub fn git_status_probe_action() -> Action {
    Action::RunCliProbe(vec!["status".into(), "--short".into(), "--branch".into()])
}

pub fn is_branch_speed_search_char(c: char) -> bool {
    (c.is_alphabetic() || matches!(c, '-' | '_' | '/' | '0' | '5'..='9')) && c != 'J'
}

pub fn is_amend_diff(state: &AppState) -> bool {
    state
        .stage_workbench
        .current_diff_key()
        .is_some_and(|k| k.side == StageDiffSide::AmendCommit)
}

pub fn is_log_diff_zoomed(state: &AppState) -> bool {
    state.active_tab == WorkspaceTab::Log && state.log.details.diff.zoomed
}

pub fn is_diff_focused(state: &AppState) -> bool {
    match state.active_tab {
        WorkspaceTab::Log => {
            state.log.details.diff.zoomed
                || (state.focused_pane == PaneId::LogDetailsDiff && state.log.details.diff_preview)
        }
        WorkspaceTab::Commit => state.focused_pane == PaneId::CommitDiffPreview,
        WorkspaceTab::Stash => state.focused_pane == PaneId::StashDiffPreview,
        WorkspaceTab::Console => false,
    }
}

/// Ordered list of keymap scopes active for the current tab/focus (highest priority first).
pub fn scopes_for_context(state: &AppState) -> Vec<KeymapScope> {
    let mut scopes = Vec::with_capacity(4);
    if is_diff_focused(state) {
        scopes.push(KeymapScope::Diff);
    }
    if !is_log_diff_zoomed(state) {
        match state.active_tab {
            WorkspaceTab::Log => {
                if state.focused_pane == PaneId::LogBranchesTree {
                    scopes.push(KeymapScope::Branch);
                }
                scopes.push(KeymapScope::Log);
            }
            WorkspaceTab::Commit => scopes.push(KeymapScope::Commit),
            WorkspaceTab::Stash => scopes.push(KeymapScope::Stash),
            WorkspaceTab::Console => scopes.push(KeymapScope::Console),
        }
        scopes.push(KeymapScope::Global);
    }
    scopes
}

pub fn check_scopes_filtered<'a, F>(
    events: &[KeyEvent],
    state: &'a AppState,
    mut extra_allow: F,
) -> MatchResult<'a>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
    let keymap = &state.config.keymap;
    let scopes = scopes_for_context(state);

    for scope in scopes {
        match match_bindings_filtered(keymap.bindings_for(scope), events, |b| {
            is_action_applicable(&b.run, state) && extra_allow(b)
        }) {
            MatchResult::None => {}
            other => return other,
        }
    }

    if is_log_diff_zoomed(state) {
        return match_bindings_filtered(&keymap.global.key_bindings, events, |b| {
            matches!(b.run, KeyAction::Quit | KeyAction::Help) && extra_allow(b)
        });
    }

    MatchResult::None
}

pub fn check_scopes<'a>(events: &[KeyEvent], state: &'a AppState) -> MatchResult<'a> {
    check_scopes_filtered(events, state, |_| true)
}

fn route_dynamic_inner<F>(
    key: &KeyEvent,
    state: &AppState,
    mut extra_allow: F,
) -> Option<Vec<Action>>
where
    F: FnMut(&KeyBindingConfig) -> bool,
{
    let expired = state
        .pending_key_at
        .is_some_and(|t| t.elapsed() > KEY_CHORD_WINDOW);
    let active_pending: &[KeyEvent] = if expired { &[] } else { &state.pending_keys };
    let had_pending = !state.pending_keys.is_empty();

    if !active_pending.is_empty() && key.code == KeyCode::Esc {
        return Some(vec![Action::ClearPendingKeySequence]);
    }

    let mut candidate = active_pending.to_vec();
    candidate.push(*key);

    let finish_exact = |run: &KeyAction| -> Option<Vec<Action>> {
        let mut actions = execute_action(run, state)?;
        if had_pending {
            actions.insert(0, Action::ClearPendingKeySequence);
        }
        Some(actions)
    };

    match check_scopes_filtered(&candidate, state, &mut extra_allow) {
        MatchResult::Exact(binding) => finish_exact(&binding.run),
        MatchResult::Prefix => Some(vec![Action::SetPendingKeySequence(candidate)]),
        MatchResult::None => {
            if candidate.len() > 1 {
                let single = vec![*key];
                match check_scopes_filtered(&single, state, &mut extra_allow) {
                    MatchResult::Exact(binding) => finish_exact(&binding.run),
                    MatchResult::Prefix => Some(vec![Action::SetPendingKeySequence(single)]),
                    MatchResult::None => None,
                }
            } else {
                None
            }
        }
    }
}

/// Whether the built-in plain `c` -> `CopyFilePath` fallback in `LogChangesTree`
/// is active (i.e. `copy_file_path` is not explicitly bound in `[log]`/`[global]`
/// and plain `c` has not been explicitly unbound via `noop` in those scopes).
pub fn is_plain_c_copy_file_path_fallback_active(state: &AppState) -> bool {
    let km = &state.config.keymap;
    if km
        .primary_key_for(
            &[KeymapScope::Log, KeymapScope::Global],
            &KeyAction::CopyFilePath,
        )
        .is_some()
    {
        return false;
    }
    let plain_c = KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::NONE);
    let explicitly_noop = [KeymapScope::Log, KeymapScope::Global]
        .iter()
        .any(|&scope| {
            matches!(
                match_bindings_filtered(km.bindings_for(scope), &[plain_c], |b| {
                    matches!(b.run, KeyAction::Noop)
                }),
                MatchResult::Exact(_)
            )
        });
    !explicitly_noop
}

/// Routes an incoming key through the user-configured keymap rules (pure read).
pub fn route_dynamic_key(key: &KeyEvent, state: &AppState) -> Option<Vec<Action>> {
    if let Some(actions) = route_dynamic_inner(key, state, |_| true) {
        return Some(actions);
    }
    // `c` in `[log]` is bound to `checkout` (inapplicable in `LogChangesTree`); avoid duplicating
    // chord `"c"` in `[log]` while still dispatching `CopyFilePath` directly on plain `c` unless
    // the user explicitly bound `copy_file_path` or unbound `c` with `noop`.
    let expired = state
        .pending_key_at
        .is_some_and(|t| t.elapsed() > KEY_CHORD_WINDOW);
    let active_pending_empty = expired || state.pending_keys.is_empty();
    let had_pending = !state.pending_keys.is_empty();
    if state.active_tab == WorkspaceTab::Log
        && state.focused_pane == PaneId::LogChangesTree
        && !is_log_diff_zoomed(state)
        && active_pending_empty
        && key.code == KeyCode::Char('c')
        && !key.modifiers.intersects(
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
        )
        && is_plain_c_copy_file_path_fallback_active(state)
    {
        let mut actions = execute_action(&KeyAction::CopyFilePath, state)?;
        if had_pending {
            actions.insert(0, Action::ClearPendingKeySequence);
        }
        return Some(actions);
    }
    None
}

/// Routes only non-typing modifier/function/tab chords when inside `CommitMessageEditor`
/// so plain character typing is never swallowed while user-remapped shortcuts still work.
pub fn route_editor_dynamic_key(key: &KeyEvent, state: &AppState) -> Option<Vec<Action>> {
    if state.pending_keys.is_empty() {
        let is_chord = key.modifiers.intersects(
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
        ) || matches!(key.code, KeyCode::Tab | KeyCode::BackTab | KeyCode::F(_));
        if !is_chord {
            return None;
        }
    }
    route_dynamic_inner(key, state, |b| {
        b.parsed_seq()
            .is_some_and(KeySequence::starts_with_editor_chord)
    })
}
