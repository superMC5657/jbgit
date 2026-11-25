//! Which-Key prefix completion hints and searchable/executable command-help rows.

use crossterm::event::KeyModifiers;

use crate::app::AppState;
use crate::config::keymap_config::{KeyBindingConfig, KeymapScope};

use super::actions::{is_action_applicable, scopes_for_context};
use super::catalog::{self, ContextMode};
use super::{KEY_CHORD_WINDOW, KeyAction, KeyStroke};

pub use super::catalog::localized_action_desc;

/// Returns `(next_key_display, description)` pairs for the currently pending prefix sequence.
pub fn pending_completions(state: &AppState) -> Vec<(String, String)> {
    if state.pending_keys.is_empty() {
        return Vec::new();
    }
    if state
        .pending_key_at
        .is_some_and(|t| t.elapsed() > KEY_CHORD_WINDOW)
    {
        return Vec::new();
    }

    let km = &state.config.keymap;
    let prefix_len = state.pending_keys.len();
    let mut out: Vec<(String, String)> = Vec::new();
    for scope in scopes_for_context(state) {
        for b in km.bindings_for(scope) {
            if !is_action_applicable(&b.run, state) {
                continue;
            }
            if let Some(seq) = b.parsed_seq()
                && seq.matches_prefix(&state.pending_keys)
                && let Some(next_stroke) = seq.strokes.get(prefix_len)
            {
                let next_disp = next_stroke.to_display_string();
                if !out.iter().any(|(k, _)| k == &next_disp) {
                    let desc = localized_action_desc(state, &b.run, b.desc.as_deref());
                    out.push((next_disp, desc));
                }
            }
        }
    }
    out
}

pub fn pending_prefix_display(state: &AppState) -> String {
    state
        .pending_keys
        .iter()
        .map(|ev| {
            let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
            let stroke = KeyStroke {
                code: ev.code,
                ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
                alt: ev.modifiers.contains(KeyModifiers::ALT),
                shift,
            };
            stroke.to_display_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpRow {
    Section(String),
    Entry {
        keys: String,
        desc: String,
        scope_tag: String,
        run: Option<KeyAction>,
    },
}

impl HelpRow {
    pub fn is_selectable(&self) -> bool {
        matches!(self, Self::Entry { .. })
    }
}

fn scope_label_key(scope: KeymapScope) -> &'static str {
    match scope {
        KeymapScope::Global => "help.scope.global",
        KeymapScope::Log => "help.scope.log",
        KeymapScope::Commit => "help.scope.commit",
        KeymapScope::Branch => "help.scope.branch",
        KeymapScope::Diff => "help.scope.diff",
        KeymapScope::Stash => "help.scope.stash",
        KeymapScope::Console => "help.scope.console",
    }
}

fn collect_merged_entries(
    state: &AppState,
    bindings: &[KeyBindingConfig],
    scope_tag: &str,
    filter_applicable: bool,
) -> Vec<HelpRow> {
    let mut order: Vec<KeyAction> = Vec::new();
    let mut keys_by_run: Vec<(KeyAction, Vec<String>, Option<String>)> = Vec::new();

    for b in bindings {
        if !b.run.is_active() {
            continue;
        }
        if filter_applicable && !is_action_applicable(&b.run, state) {
            continue;
        }
        let Some(key_disp) = b.display_key() else {
            continue;
        };
        let key_disp = key_disp.to_string();
        if let Some(pos) = order.iter().position(|r| r == &b.run) {
            let entry = &mut keys_by_run[pos];
            if !entry.1.contains(&key_disp) {
                entry.1.push(key_disp);
            }
            if entry.2.is_none() {
                entry.2 = b.desc.clone();
            }
        } else {
            order.push(b.run.clone());
            keys_by_run.push((b.run.clone(), vec![key_disp], b.desc.clone()));
        }
    }

    keys_by_run
        .into_iter()
        .map(|(run, keys, fallback_desc)| HelpRow::Entry {
            keys: keys.join(" / "),
            desc: localized_action_desc(state, &run, fallback_desc.as_deref()),
            scope_tag: scope_tag.to_string(),
            run: Some(run),
        })
        .collect()
}

fn current_context_rows(state: &AppState) -> Vec<HelpRow> {
    let km = &state.config.keymap;
    let i18n = &state.i18n;
    let pane_title = i18n.pane_title(state.focused_pane);
    let mut out = Vec::new();

    if let ContextMode::CommitEditor { editing_author } = catalog::context_mode(state) {
        for action in catalog::commit_editor_actions() {
            let keys = km.editor_keys_for(&[KeymapScope::Commit], action);
            if !keys.is_empty() {
                out.push(HelpRow::Entry {
                    keys: keys.join(" / "),
                    desc: localized_action_desc(state, action, None),
                    scope_tag: pane_title.to_string(),
                    run: Some(action.clone()),
                });
            }
        }
        let (esc_hint, _) = catalog::commit_editor_structural_hints(state, editing_author);
        out.push(HelpRow::Entry {
            keys: esc_hint.key,
            desc: esc_hint.label,
            scope_tag: pane_title.to_string(),
            run: None,
        });
        return out;
    }

    let structural = catalog::normal_structural_hints(state);
    if let Some(probe) = structural.console_probe_fallback {
        out.push(HelpRow::Entry {
            keys: probe.key,
            desc: localized_action_desc(state, &KeyAction::GitStatusProbe, None),
            scope_tag: pane_title.to_string(),
            run: probe.action,
        });
    }

    for scope in scopes_for_context(state) {
        if scope == KeymapScope::Global {
            continue;
        }
        out.extend(collect_merged_entries(
            state,
            km.bindings_for(scope),
            i18n.t(scope_label_key(scope)),
            true,
        ));
    }

    if let Some(trailing) = structural.help_trailing {
        out.push(HelpRow::Entry {
            keys: trailing.key,
            desc: trailing.label,
            scope_tag: pane_title.to_string(),
            run: trailing.action,
        });
    }

    // Deduplicate by `run` preserving first occurrence (higher-priority scope wins).
    let mut seen: Vec<KeyAction> = Vec::new();
    out.retain(|row| match row {
        HelpRow::Entry { run: Some(r), .. } => {
            if seen.contains(r) {
                false
            } else {
                seen.push(r.clone());
                true
            }
        }
        _ => true,
    });
    out
}

fn advanced_tool_rows(state: &AppState) -> Vec<HelpRow> {
    let scope = state.i18n.t("help.scope.advanced").to_string();
    let all_scopes = &[
        KeymapScope::Global,
        KeymapScope::Log,
        KeymapScope::Commit,
        KeymapScope::Branch,
        KeymapScope::Diff,
        KeymapScope::Stash,
        KeymapScope::Console,
    ];
    catalog::advanced_tool_actions()
        .iter()
        .map(|&(run, default_key)| {
            let keys = catalog::shortcut_for_action(state, all_scopes, run)
                .unwrap_or_else(|| default_key.to_string());
            HelpRow::Entry {
                keys,
                desc: localized_action_desc(state, run, None),
                scope_tag: scope.clone(),
                run: Some(run.clone()),
            }
        })
        .collect()
}

pub fn build_help_rows(state: &AppState, query: &str) -> Vec<HelpRow> {
    let i18n = &state.i18n;
    let km = &state.config.keymap;
    let q = query.trim().to_lowercase();

    let ctx_rows = current_context_rows(state);
    let global_rows = collect_merged_entries(
        state,
        &km.global.key_bindings,
        i18n.t("help.scope.global"),
        false,
    );
    let commit_rows = collect_merged_entries(
        state,
        &km.commit.key_bindings,
        i18n.t("help.scope.commit"),
        false,
    );
    let log_rows =
        collect_merged_entries(state, &km.log.key_bindings, i18n.t("help.scope.log"), false);
    let branch_rows = collect_merged_entries(
        state,
        &km.branch.key_bindings,
        i18n.t("help.scope.branch"),
        false,
    );
    let diff_rows = collect_merged_entries(
        state,
        &km.diff.key_bindings,
        i18n.t("help.scope.diff"),
        false,
    );
    let stash_rows = collect_merged_entries(
        state,
        &km.stash.key_bindings,
        i18n.t("help.scope.stash"),
        false,
    );
    let console_rows = collect_merged_entries(
        state,
        &km.console.key_bindings,
        i18n.t("help.scope.console"),
        false,
    );
    let adv_rows = advanced_tool_rows(state);
    let other_rows = vec![
        HelpRow::Entry {
            keys: i18n.t("help.mouse_drag_key").to_string(),
            desc: i18n.t("help.mouse_drag").to_string(),
            scope_tag: i18n.t("help.scope.global").to_string(),
            run: None,
        },
        HelpRow::Entry {
            keys: i18n.t("help.right_click_key").to_string(),
            desc: i18n.t("help.right_click").to_string(),
            scope_tag: i18n.t("help.scope.global").to_string(),
            run: None,
        },
    ];

    if q.is_empty() {
        let mut rows = Vec::new();
        let pane_title = i18n.pane_title(state.focused_pane);
        if !ctx_rows.is_empty() {
            rows.push(HelpRow::Section(
                i18n.t_fmt("help.current_context", &[("pane", pane_title)]),
            ));
            rows.extend(ctx_rows);
        }
        for (title, sec_rows) in [
            (i18n.t("help.section_global"), global_rows),
            (i18n.t("help.section_commit"), commit_rows),
            (i18n.t("help.section_log"), log_rows),
            (i18n.t("help.section_branch"), branch_rows),
            (i18n.t("help.section_diff"), diff_rows),
            (i18n.t("help.section_stash"), stash_rows),
            (i18n.t("help.section_console"), console_rows),
            (i18n.t("help.section_advanced"), adv_rows),
            (i18n.t("help.section_other"), other_rows),
        ] {
            if !sec_rows.is_empty() {
                rows.push(HelpRow::Section(title.to_string()));
                rows.extend(sec_rows);
            }
        }
        return rows;
    }

    let mut matched: Vec<(u8, HelpRow)> = Vec::new();
    let mut seen_runs: Vec<(Option<KeyAction>, String)> = Vec::new();
    for row in ctx_rows
        .into_iter()
        .chain(global_rows)
        .chain(commit_rows)
        .chain(log_rows)
        .chain(branch_rows)
        .chain(diff_rows)
        .chain(stash_rows)
        .chain(console_rows)
        .chain(adv_rows)
        .chain(other_rows)
    {
        if let HelpRow::Entry {
            ref keys,
            ref desc,
            ref scope_tag,
            ref run,
        } = row
        {
            let run_lc = run.as_ref().map_or("", KeyAction::as_str).to_lowercase();
            let keys_lc = keys.to_lowercase();
            let desc_lc = desc.to_lowercase();
            let scope_lc = scope_tag.to_lowercase();
            let rank = if run_lc == q || keys_lc == q {
                Some(0u8)
            } else if run_lc.starts_with(&q) || desc_lc.starts_with(&q) {
                Some(1)
            } else if run_lc.contains(&q) || desc_lc.contains(&q) || keys_lc.contains(&q) {
                Some(2)
            } else if scope_lc.contains(&q) {
                Some(3)
            } else {
                None
            };
            if let Some(score) = rank {
                let dedup_key = (run.clone(), desc.clone());
                if !seen_runs.contains(&dedup_key) {
                    seen_runs.push(dedup_key);
                    matched.push((score, row));
                }
            }
        }
    }
    matched.sort_by_key(|(score, _)| *score);
    matched.into_iter().map(|(_, row)| row).collect()
}

pub fn first_help_entry(rows: &[HelpRow]) -> usize {
    rows.iter().position(HelpRow::is_selectable).unwrap_or(0)
}

pub fn resolve_help_selection(rows: &[HelpRow], selected: usize) -> usize {
    if rows.get(selected).is_some_and(HelpRow::is_selectable) {
        selected
    } else {
        first_help_entry(rows)
    }
}

pub fn step_help_selection(rows: &[HelpRow], from: usize, delta: isize) -> usize {
    let n = rows.len();
    if n == 0 || !rows.iter().any(HelpRow::is_selectable) {
        return 0;
    }
    let mut idx = resolve_help_selection(rows, from);
    let step = if delta >= 0 { 1isize } else { -1isize };
    let count = delta.unsigned_abs().max(1);
    for _ in 0..count {
        for _ in 0..n {
            idx = (idx as isize + step).rem_euclid(n as isize) as usize;
            if rows[idx].is_selectable() {
                break;
            }
        }
    }
    idx
}
