use crate::app::{AppState, Effect};
use crate::terminal::TerminalGuard;

pub(crate) fn effects_need_terminal_handoff(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::OpenExternalEditorMessage
                | Effect::OpenExternalEditorFile(_)
                | Effect::OpenExternalMergetool(_)
        )
    })
}

fn resume_terminal_after_external_cmd(
    term_guard: &mut Option<TerminalGuard>,
    state: &mut AppState,
) {
    *term_guard = TerminalGuard::enter().ok();
    state.graph_renderer.invalidate();
}

pub(crate) fn is_external_editor_disabled_val(val: Option<&str>) -> bool {
    val.is_some_and(|v| {
        let trimmed = v.trim();
        trimmed == "1"
            || trimmed.eq_ignore_ascii_case("true")
            || trimmed.eq_ignore_ascii_case("yes")
    })
}

fn is_external_editor_disabled() -> bool {
    is_external_editor_disabled_val(std::env::var("JBGIT_NO_EXTERNAL_EDITOR").ok().as_deref())
}

pub(crate) fn split_editor_command(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' && chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    current.push(c);
                }
            }
            _ => {
                if c.is_whitespace() {
                    if in_token {
                        parts.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                    in_token = true;
                } else {
                    current.push(c);
                    in_token = true;
                }
            }
        }
    }
    if in_token {
        parts.push(current);
    }

    let mut iter = parts.into_iter();
    let prog = iter.next().filter(|s| !s.is_empty())?;
    Some((prog, iter.collect()))
}

pub(crate) fn resolve_editor_command_with<F>(get_env: F) -> (String, Vec<String>)
where
    F: Fn(&str) -> Option<String>,
{
    for var in ["GIT_EDITOR", "VISUAL", "EDITOR"] {
        if let Some(raw) = get_env(var)
            && !raw.trim().is_empty()
            && let Some(cmd) = split_editor_command(&raw)
        {
            return cmd;
        }
    }
    let fallback = if cfg!(windows) { "notepad" } else { "vi" };
    (fallback.to_string(), Vec::new())
}

fn set_editor_failed_notice(state: &mut AppState, err: &str) {
    state.status_notice = Some(
        state
            .i18n
            .t_fmt("toast.external_editor_failed", &[("error", err)]),
    );
}

fn run_external_editor_command(
    bin: &str,
    args: &[String],
    path: &std::path::Path,
    state: &mut AppState,
    term_guard: &mut Option<TerminalGuard>,
) -> Result<(), String> {
    if bin == ":" || bin == "true" {
        return Ok(());
    }
    drop(term_guard.take());
    let status = std::process::Command::new(bin)
        .args(args)
        .arg(path)
        .status();
    resume_terminal_after_external_cmd(term_guard, state);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("exit {}", s.code().unwrap_or(-1))),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn handle_external_editor_effects(
    effects: &[Effect],
    state: &mut AppState,
    term_guard: &mut Option<TerminalGuard>,
) -> bool {
    handle_external_editor_effects_with(
        effects,
        state,
        term_guard,
        is_external_editor_disabled(),
        || resolve_editor_command_with(|k| std::env::var(k).ok()),
    )
}

pub(crate) fn handle_external_editor_effects_with<F>(
    effects: &[Effect],
    state: &mut AppState,
    term_guard: &mut Option<TerminalGuard>,
    disabled: bool,
    resolve_editor: F,
) -> bool
where
    F: Fn() -> (String, Vec<String>),
{
    let mut refresh_stage = false;

    for eff in effects {
        match eff {
            Effect::OpenExternalEditorMessage => {
                if disabled {
                    state.status_notice =
                        Some(state.i18n.t("toast.external_editor_disabled").to_string());
                    state.needs_redraw = true;
                    continue;
                }
                // The editor may run for minutes: persist the current draft first.
                state.stage_workbench.flush_draft_sync();
                let tmp_path = std::env::temp_dir()
                    .join(format!("jbgit-commit-editmsg-{}.txt", std::process::id()));
                if let Err(e) = std::fs::write(&tmp_path, state.stage_workbench.editor.text()) {
                    set_editor_failed_notice(state, &e.to_string());
                    state.needs_redraw = true;
                    continue;
                }
                let (bin, args) = resolve_editor();
                match run_external_editor_command(&bin, &args, &tmp_path, state, term_guard) {
                    Ok(()) => {
                        if let Ok(updated) = std::fs::read_to_string(&tmp_path) {
                            let prev = state.stage_workbench.editor.text();
                            if !state.stage_workbench.amend
                                && !prev.trim().is_empty()
                                && prev.trim() != updated.trim()
                            {
                                state.stage_workbench.push_history(&prev);
                            }
                            state.stage_workbench.editor.set_text(&updated);
                            state.stage_workbench.empty_message_error = false;
                            state.stage_workbench.mark_draft_dirty(std::time::Instant::now());
                        }
                    }
                    Err(err) => {
                        set_editor_failed_notice(state, &err);
                    }
                }
                let _ = std::fs::remove_file(&tmp_path);
                state.needs_redraw = true;
            }
            Effect::OpenExternalEditorFile(rel) => {
                if disabled {
                    state.status_notice =
                        Some(state.i18n.t("toast.external_editor_disabled").to_string());
                    state.needs_redraw = true;
                    continue;
                }
                let full = state.repo_path_display.join(rel);
                let (bin, args) = resolve_editor();
                if let Err(err) = run_external_editor_command(&bin, &args, &full, state, term_guard)
                {
                    set_editor_failed_notice(state, &err);
                }
                state.needs_redraw = true;
                refresh_stage = true;
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
