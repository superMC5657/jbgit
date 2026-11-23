//! `Apply Patch` (Spec 12 Issue 13, `ApplyPatchAction` /
//! `ApplyPatchDifferentiatedDialog`): a read-only `git apply --check
//! --numstat` preflight lists the affected files for the chosen mode
//! (working tree / index / 3-way); Apply runs [`GitOp::ApplyPatch`].
//! Also hosts the patch path completion of the "Apply Patch…" input.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{ApplyMode, GitOp, PatchCheck, PatchCheckKey, PatchInput};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{Action, AppState, ModalLayer, RenderContext};
use crate::hit_test::HitTarget;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// State of [`ModalLayer::ApplyPatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPatchState {
    pub source: PatchInput,
    pub mode: ApplyMode,
    /// Preflight of `key()`; `None` while loading.
    pub check: Option<Arc<PatchCheck>>,
    pub scroll: usize,
}

impl ApplyPatchState {
    pub fn new(source: PatchInput) -> Self {
        Self {
            source,
            mode: ApplyMode::default(),
            check: None,
            scroll: 0,
        }
    }

    pub fn key(&self) -> PatchCheckKey {
        PatchCheckKey {
            source: self.source.clone(),
            mode: self.mode,
        }
    }

    /// The preflight for the current mode, once it arrived.
    pub fn current_check(&self) -> Option<&PatchCheck> {
        self.check.as_deref().filter(|c| c.key == self.key())
    }

    pub fn can_apply(&self) -> bool {
        self.current_check().is_some_and(PatchCheck::applies)
    }
}

fn top(state: &AppState) -> Option<&ApplyPatchState> {
    match state.modal_stack.last() {
        Some(ModalLayer::ApplyPatch(s)) => Some(s.as_ref()),
        _ => None,
    }
}

/// Actions of the Apply button (`None` while it is disabled, with a notice).
pub fn confirm_actions(state: &AppState) -> Vec<Action> {
    let Some(s) = top(state) else {
        return Vec::new();
    };
    if !s.can_apply() {
        let key = if s.current_check().is_some() {
            "dialog.apply_patch.cannot_apply"
        } else {
            "dialog.apply_patch.checking"
        };
        return vec![Action::Notice(state.i18n.t(key).to_string())];
    }
    vec![
        Action::PopModal,
        Action::RunGitOp(GitOp::ApplyPatch {
            source: s.source.clone(),
            mode: s.mode,
        }),
    ]
}

/// Keyboard routing while the Apply Patch modal is on top.
pub fn route_apply_patch_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
        KeyCode::Enter | KeyCode::Char('a') => confirm_actions(state),
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('m') => {
            vec![Action::ApplyPatchSetMode(None)]
        }
        KeyCode::BackTab | KeyCode::Left => {
            let prev = top(state).map_or(ApplyMode::WorkingTree, |s| s.mode.next().next());
            vec![Action::ApplyPatchSetMode(Some(prev))]
        }
        KeyCode::Char('1') => vec![Action::ApplyPatchSetMode(Some(ApplyMode::WorkingTree))],
        KeyCode::Char('2') => vec![Action::ApplyPatchSetMode(Some(ApplyMode::Index))],
        KeyCode::Char('3') => vec![Action::ApplyPatchSetMode(Some(ApplyMode::ThreeWay))],
        KeyCode::Down | KeyCode::Char('j') => vec![Action::ApplyPatchScroll(1)],
        KeyCode::Up | KeyCode::Char('k') => vec![Action::ApplyPatchScroll(-1)],
        _ => Vec::new(),
    }
}

/// Tab completion of a patch path typed relative to `base` (or absolute):
/// extends to the longest common prefix of matching entries, appending `/`
/// for a single matching directory.
pub fn complete_path(base: &Path, value: &str) -> Option<String> {
    let (dir_part, prefix) = match value.rfind(['/', '\\']) {
        Some(i) => (&value[..=i], &value[i + 1..]),
        None => ("", value),
    };
    let dir: PathBuf = if dir_part.is_empty() {
        base.to_path_buf()
    } else if Path::new(dir_part).is_absolute() {
        PathBuf::from(dir_part)
    } else {
        base.join(dir_part)
    };
    let mut matches: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = |s: &str| {
                if cfg!(windows) {
                    s.to_lowercase()
                } else {
                    s.to_string()
                }
            };
            lower(&name)
                .starts_with(&lower(prefix))
                .then(|| (name, e.path().is_dir()))
        })
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort();
    let completed = if let [(name, is_dir)] = matches.as_slice() {
        if *is_dir {
            format!("{name}/")
        } else {
            name.clone()
        }
    } else {
        let first = &matches[0].0;
        let mut end = first.len();
        for (m, _) in &matches[1..] {
            end = first
                .char_indices()
                .zip(m.chars())
                .take_while(|((_, a), b)| {
                    if cfg!(windows) {
                        a.eq_ignore_ascii_case(b)
                    } else {
                        a == b
                    }
                })
                .last()
                .map_or(0, |((i, a), _)| i + a.len_utf8())
                .min(end);
        }
        first[..end].to_string()
    };
    let next = format!("{dir_part}{completed}");
    (next != value).then_some(next)
}

pub fn render_apply_patch_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    s: &ApplyPatchState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 30 || screen.height < 10 {
        return;
    }
    let check = s.current_check();
    let rows = check.map_or(1, |c| c.files.len().max(1)) as u16;
    let w = screen.width.saturating_sub(4).clamp(30, 100);
    let h = (rows + 9)
        .clamp(11, 28)
        .min(screen.height.saturating_sub(2));
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", ctx.t("dialog.apply_patch.title")),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);
    let width = inner.width as usize;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let line = |frame: &mut Frame<'_>, y: u16, l: Line<'static>| {
        if y < inner.bottom() {
            frame.render_widget(Paragraph::new(l), Rect::new(inner.x, y, inner.width, 1));
        }
    };

    // Source.
    let source = match &s.source {
        PatchInput::File(p) => p.display().to_string(),
        PatchInput::Clipboard => ctx.t("dialog.apply_patch.clipboard").to_string(),
    };
    line(
        frame,
        inner.y,
        Line::from(vec![
            Span::styled(format!(" {} ", ctx.t("dialog.apply_patch.source")), muted),
            Span::styled(
                truncate_to_width(&source, width.saturating_sub(12)),
                Style::default().fg(ctx.theme.fg_primary),
            ),
        ]),
    );

    // Mode radio row.
    let mut x = inner.x + 1;
    let y = inner.y + 2;
    let label = format!("{} ", ctx.t("dialog.apply_patch.mode"));
    frame.render_widget(
        Paragraph::new(Span::styled(label.clone(), muted)),
        Rect::new(x, y, label.chars().count() as u16, 1),
    );
    x += label.chars().count() as u16;
    for (i, mode) in ApplyMode::ALL.into_iter().enumerate() {
        let on = mode == s.mode;
        let name = ctx.t(match mode {
            ApplyMode::WorkingTree => "dialog.apply_patch.mode_worktree",
            ApplyMode::Index => "dialog.apply_patch.mode_index",
            ApplyMode::ThreeWay => "dialog.apply_patch.mode_3way",
        });
        let text = format!("{} {}:{name}  ", if on { "●" } else { "○" }, i + 1);
        let tw = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
        if x + tw > inner.right() {
            break;
        }
        let style = if on {
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.fg_primary)
        };
        let r = Rect::new(x, y, tw, 1);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), r);
        ctx.hit_registry
            .register(r, z + 2, HitTarget::ApplyPatchMode(mode));
        x += tw;
    }

    // Files / error.
    let list_top = inner.y + 4;
    let list_h = inner.bottom().saturating_sub(list_top + 2) as usize;
    match check {
        None => line(
            frame,
            list_top,
            Line::from(Span::styled(
                format!(" {}", ctx.t("dialog.apply_patch.checking")),
                muted,
            )),
        ),
        Some(c) if !c.applies() => {
            let err = c.error.clone().unwrap_or_default();
            line(
                frame,
                list_top,
                Line::from(Span::styled(
                    truncate_to_width(&format!(" ✗ {err}"), width),
                    Style::default().fg(ctx.theme.status_conflicted),
                )),
            );
            if s.mode != ApplyMode::ThreeWay {
                line(
                    frame,
                    list_top + 1,
                    Line::from(Span::styled(
                        truncate_to_width(
                            &format!(" {}", ctx.t("dialog.apply_patch.try_3way")),
                            width,
                        ),
                        muted,
                    )),
                );
            }
        }
        Some(c) => {
            let first = s.scroll.min(c.files.len().saturating_sub(list_h.max(1)));
            for (row, f) in c.files.iter().skip(first).take(list_h).enumerate() {
                let counts = match (f.added, f.deleted) {
                    (Some(a), Some(d)) => vec![
                        Span::styled(format!("+{a}"), Style::default().fg(ctx.theme.status_added)),
                        Span::styled(" ", muted),
                        Span::styled(
                            format!("-{d}"),
                            Style::default().fg(ctx.theme.status_deleted),
                        ),
                    ],
                    _ => vec![Span::styled(
                        ctx.t("dialog.apply_patch.binary").to_string(),
                        muted,
                    )],
                };
                let mut spans = vec![Span::styled(
                    format!(
                        " {:<w$} ",
                        truncate_to_width(&f.path, width.saturating_sub(24)),
                        w = width.saturating_sub(24).min(60)
                    ),
                    Style::default().fg(ctx.theme.fg_primary),
                )];
                spans.extend(counts);
                if f.conflicts {
                    spans.push(Span::styled(
                        format!("  {}", ctx.t("dialog.apply_patch.will_conflict")),
                        Style::default().fg(ctx.theme.status_conflicted),
                    ));
                }
                line(frame, list_top + row as u16, Line::from(spans));
            }
        }
    }

    // Summary + buttons.
    let summary = match check {
        Some(c) if c.applies() => {
            let n = c.files.len().to_string();
            let conflicts = c.conflict_count();
            if conflicts > 0 {
                ctx.t_fmt(
                    "dialog.apply_patch.summary_conflicts",
                    &[("count", &n), ("conflicts", &conflicts.to_string())],
                )
            } else {
                ctx.t_fmt("dialog.apply_patch.summary", &[("count", &n)])
            }
        }
        _ => String::new(),
    };
    let by = inner.bottom().saturating_sub(1);
    line(
        frame,
        by.saturating_sub(1),
        Line::from(Span::styled(
            truncate_to_width(&format!(" {summary}"), width),
            muted,
        )),
    );
    let apply = format!("[ {} ]", ctx.t("dialog.apply_patch.apply_btn"));
    let cancel = format!("[ {} ]", ctx.t("common.cancel"));
    let aw = unicode_width::UnicodeWidthStr::width(apply.as_str()) as u16;
    let cw = unicode_width::UnicodeWidthStr::width(cancel.as_str()) as u16;
    let cx = inner.right().saturating_sub(cw + 1);
    let ax = cx.saturating_sub(aw + 1);
    let apply_style = if s.can_apply() {
        Style::default()
            .fg(ctx.theme.fg_bright)
            .bg(ctx.theme.border_focused)
            .add_modifier(Modifier::BOLD)
    } else {
        muted
    };
    let ar = Rect::new(ax, by, aw, 1);
    let cr = Rect::new(cx, by, cw, 1);
    frame.render_widget(Paragraph::new(Span::styled(apply, apply_style)), ar);
    frame.render_widget(
        Paragraph::new(Span::styled(
            cancel,
            Style::default().fg(ctx.theme.fg_primary),
        )),
        cr,
    );
    ctx.hit_registry
        .register(ar, z + 2, HitTarget::ApplyPatchButton { apply: true });
    ctx.hit_registry
        .register(cr, z + 2, HitTarget::ApplyPatchButton { apply: false });
    let hint = ctx.t("dialog.apply_patch.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), ax.saturating_sub(inner.x) as usize),
            muted,
        )),
        Rect::new(inner.x, by, ax.saturating_sub(inner.x), 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_completion_extends_common_prefix_and_marks_directories() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("patches")).unwrap();
        std::fs::write(dir.path().join("patches/fix-a.patch"), "").unwrap();
        std::fs::write(dir.path().join("patches/fix-b.patch"), "").unwrap();
        std::fs::write(dir.path().join("other.diff"), "").unwrap();
        assert_eq!(complete_path(dir.path(), "pa").as_deref(), Some("patches/"));
        assert_eq!(
            complete_path(dir.path(), "patches/f").as_deref(),
            Some("patches/fix-")
        );
        assert_eq!(
            complete_path(dir.path(), "patches/fix-a").as_deref(),
            Some("patches/fix-a.patch")
        );
        assert_eq!(complete_path(dir.path(), "zzz"), None);
        assert_eq!(complete_path(dir.path(), "patches/fix-"), None);
    }
}
