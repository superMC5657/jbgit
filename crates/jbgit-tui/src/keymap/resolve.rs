//! Keymap conflict, shadowing, and display-key resolution for footer hints.

use crossterm::event::KeyCode;

use crate::app::AppState;
use crate::config::{KeyBindingConfig, KeymapScope};

use super::actions::{is_action_applicable, is_branch_speed_search_char};
use super::{KeyAction, KeySequence};

pub(crate) struct KeyResolveCtx<'a> {
    pub state: &'a AppState,
    pub filter_speed_search: bool,
    pub editor_only: bool,
    pub claimed_keys: &'a [KeySequence],
}

pub(crate) fn is_speed_search_captured(seq: &KeySequence, filter_speed_search: bool) -> bool {
    filter_speed_search
        && seq.strokes.first().is_some_and(|s| {
            !s.ctrl
                && !s.alt
                && matches!(s.code, KeyCode::Char(c) if is_branch_speed_search_char(c))
        })
}

fn seqs_conflict(a: &KeySequence, b: &KeySequence) -> bool {
    let n = a.strokes.len().min(b.strokes.len());
    n > 0 && a.strokes[..n] == b.strokes[..n]
}

fn has_earlier_conflict(
    earlier: &[KeyBindingConfig],
    seq: &KeySequence,
    ctx: &KeyResolveCtx<'_>,
    target_run: &KeyAction,
) -> bool {
    earlier.iter().any(|prev| {
        prev.run != *target_run
            && is_action_applicable(&prev.run, ctx.state)
            && KeySequence::parse(&prev.on).is_some_and(|prev_seq| {
                (!ctx.editor_only || prev_seq.starts_with_editor_chord())
                    && !is_speed_search_captured(&prev_seq, ctx.filter_speed_search)
                    && seqs_conflict(&prev_seq, seq)
            })
    })
}

fn is_candidate_usable(
    ctx: &KeyResolveCtx<'_>,
    scopes: &[KeymapScope],
    start_scope_idx: usize,
    scope_idx: usize,
    binding_idx: usize,
    seq: &KeySequence,
    target_run: &KeyAction,
) -> bool {
    if (ctx.editor_only && !seq.starts_with_editor_chord())
        || is_speed_search_captured(seq, ctx.filter_speed_search)
        || ctx.claimed_keys.iter().any(|c| seqs_conflict(c, seq))
    {
        return false;
    }

    let km = &ctx.state.config.keymap;
    let cur_bindings = km.bindings_for(scopes[scope_idx]);
    !scopes[start_scope_idx..scope_idx]
        .iter()
        .any(|&s| has_earlier_conflict(km.bindings_for(s), seq, ctx, target_run))
        && !has_earlier_conflict(&cur_bindings[..binding_idx], seq, ctx, target_run)
}

/// Resolves the best display key string for `binding`, falling back to surviving aliases
/// of the same action across `scopes[scope_idx..]` when the primary key is shadowed.
pub(crate) fn resolve_key_in_scopes(
    ctx: &KeyResolveCtx<'_>,
    scopes: &[KeymapScope],
    scope_idx: usize,
    binding_idx: usize,
    binding: &KeyBindingConfig,
) -> Option<String> {
    if let Some(seq) = KeySequence::parse(&binding.on)
        && is_candidate_usable(
            ctx,
            scopes,
            scope_idx,
            scope_idx,
            binding_idx,
            &seq,
            &binding.run,
        )
    {
        return Some(seq.to_display_string());
    }

    let km = &ctx.state.config.keymap;
    for (rel_idx, &fb_scope) in scopes[scope_idx..].iter().enumerate() {
        let fb_scope_idx = scope_idx + rel_idx;
        for (fb_idx, fb) in km.bindings_for(fb_scope).iter().enumerate() {
            if fb.run != binding.run || fb.show_hint == Some(false) {
                continue;
            }
            if let Some(alt_seq) = KeySequence::parse(&fb.on)
                && is_candidate_usable(
                    ctx,
                    scopes,
                    scope_idx,
                    fb_scope_idx,
                    fb_idx,
                    &alt_seq,
                    &fb.run,
                )
            {
                return Some(alt_seq.to_display_string());
            }
        }
    }

    None
}

pub(crate) fn push_unique_seq(out: &mut Vec<KeySequence>, seq: KeySequence) {
    if !out.contains(&seq) {
        out.push(seq);
    }
}

pub(crate) fn collect_claimed_keys(
    bindings: &[KeyBindingConfig],
    state: &AppState,
    filter_speed_search: bool,
    claimed: &mut Vec<KeySequence>,
    mut non_global_claimed: Option<&mut Vec<KeySequence>>,
) {
    for b in bindings {
        if is_action_applicable(&b.run, state)
            && let Some(seq) = KeySequence::parse(&b.on)
            && !is_speed_search_captured(&seq, filter_speed_search)
        {
            push_unique_seq(claimed, seq.clone());
            if let Some(ng) = non_global_claimed.as_deref_mut() {
                push_unique_seq(ng, seq);
            }
        }
    }
}
