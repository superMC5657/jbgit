//! `Apply Patch` (Spec 12 Issue 13 + Spec 13 Issue 10,
//! `ApplyPatchAction` / `ApplyPatchDifferentiatedDialog` / `PatchDiffViewer`):
//! a read-only `git apply --check --numstat` preflight with `-R` (reverse),
//! `-p<n>` (strip), `--directory=<dir>`, per-file checkboxes (`--include`),
//! syntax-highlighted single-file diff preview, and `--3way` plain-patch
//! fallback guidance. Also hosts the patch path completion of the
//! "Apply Patch…" input.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{
    ApplyMode, ApplyOptions, GitOp, PatchCheck, PatchCheckKey, PatchFileSection, PatchFileStat,
    PatchInput,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext, ScrollPane, ScrollViewport};
use crate::hit_test::HitTarget;
use crate::syntax::{highlight_single_line, render_diff_line_spans_with_tab};
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// Debounce delay before firing `--check --numstat` after option/checkbox edits.
pub const PATCH_CHECK_DEBOUNCE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApplyPatchFocus {
    #[default]
    Files,
    Preview,
}

/// Sub-actions of [`Action::ApplyPatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyPatchAction {
    SetMode(Option<ApplyMode>),
    ToggleReverse,
    AdjustStrip(i8),
    StartDirEdit,
    DirEditKey(KeyEvent),
    Move(isize),
    Select(usize),
    ToggleFile(Option<usize>),
    ToggleAll,
    TogglePreview,
    OpenPreview,
    ToggleFocus,
    ScrollPreview(isize),
    Escape,
    Confirm,
}

/// State of [`ModalLayer::ApplyPatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPatchState {
    pub source: PatchInput,
    pub options: ApplyOptions,
    /// Full file list discovered from the patch (kept intact when files are unchecked).
    pub known_files: Vec<PatchFileStat>,
    /// Unchecked paths from `known_files` (new files default to checked).
    pub unchecked: HashSet<String>,
    /// Selected row index in `known_files`.
    pub selected: usize,
    /// Preflight of `key()`; `None` while loading.
    pub check: Option<Arc<PatchCheck>>,
    /// Per-file diff sections parsed from the patch text.
    pub sections: Vec<PatchFileSection>,
    /// Whether the right-hand diff preview pane is visible.
    pub show_preview: bool,
    pub focus: ApplyPatchFocus,
    pub scroll: ScrollViewport,
    pub preview_scroll: ScrollViewport,
    /// Inline `--directory` editor draft when active (`D`).
    pub editing_dir: Option<String>,
    /// Debounced `--check --numstat` deadline after option/checkbox edits.
    pub pending_check_due: Option<Instant>,
    /// Last `(effective_strip, normalized_directory)` used when populating `known_files`.
    last_path_transform: (u8, Option<String>),
}

impl ApplyPatchState {
    pub fn new(source: PatchInput) -> Self {
        let options = ApplyOptions::default();
        let last_path_transform = (options.effective_strip(), options.normalized_directory());
        Self {
            source,
            options,
            known_files: Vec::new(),
            unchecked: HashSet::new(),
            selected: 0,
            check: None,
            sections: Vec::new(),
            show_preview: false,
            focus: ApplyPatchFocus::Files,
            scroll: ScrollViewport::new(),
            preview_scroll: ScrollViewport::new(),
            editing_dir: None,
            pending_check_due: None,
            last_path_transform,
        }
    }

    pub fn key(&self) -> PatchCheckKey {
        PatchCheckKey::new(self.source.clone(), self.options.clone())
    }

    /// The preflight for the current options, once it arrived.
    pub fn current_check(&self) -> Option<&PatchCheck> {
        self.check.as_deref().filter(|c| c.key == self.key())
    }

    pub fn checked_count(&self) -> usize {
        self.known_files
            .iter()
            .filter(|f| !self.unchecked.contains(&f.path))
            .count()
    }

    pub fn all_unchecked(&self) -> bool {
        !self.known_files.is_empty() && self.checked_count() == 0
    }

    pub fn is_checked(&self, path: &str) -> bool {
        !self.unchecked.contains(path)
    }

    pub fn can_apply(&self) -> bool {
        !self.all_unchecked()
            && self.pending_check_due.is_none()
            && self.current_check().is_some_and(PatchCheck::applies)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending_check_due
    }

    pub fn schedule_check(&mut self, now: Instant) {
        if self.all_unchecked() {
            self.pending_check_due = None;
            return;
        }
        self.pending_check_due = Some(now + PATCH_CHECK_DEBOUNCE);
    }

    pub fn flush_due(&mut self, now: Instant) -> Option<PatchCheckKey> {
        let due = self.pending_check_due?;
        if now < due {
            return None;
        }
        self.pending_check_due = None;
        (!self.all_unchecked()).then(|| self.key())
    }

    pub fn sync_include_paths(&mut self) {
        if self.unchecked.is_empty() || self.known_files.is_empty() {
            self.options.include_paths.clear();
            return;
        }
        self.options.include_paths = self
            .known_files
            .iter()
            .filter(|f| !self.unchecked.contains(&f.path))
            .map(|f| f.path.clone())
            .collect();
    }

    pub fn on_path_transform_changed(&mut self) {
        let next = (
            self.options.effective_strip(),
            self.options.normalized_directory(),
        );
        if next != self.last_path_transform {
            self.last_path_transform = next;
            self.unchecked.clear();
            self.options.include_paths.clear();
        }
    }

    pub fn on_check(&mut self, check: Arc<PatchCheck>) {
        let transform = (
            check.key.options.effective_strip(),
            check.key.options.normalized_directory(),
        );
        let transform_changed = transform != self.last_path_transform;
        if transform_changed {
            self.last_path_transform = transform;
            self.unchecked.clear();
        }

        if !check.sections.is_empty() {
            self.sections = check.sections.clone();
        }

        if check.key.options.include_paths.is_empty() {
            if !check.files.is_empty() {
                self.known_files = check.files.clone();
            } else if !check.sections.is_empty() && (self.known_files.is_empty() || transform_changed)
            {
                self.known_files = check
                    .sections
                    .iter()
                    .map(|sec| PatchFileStat {
                        path: sec.path.clone(),
                        added: None,
                        deleted: None,
                        conflicts: false,
                    })
                    .collect();
            }
        } else {
            // Partial preflight: keep full list intact, refresh stats/conflict flags for checked files.
            for f in &mut self.known_files {
                if let Some(updated) = check.files.iter().find(|u| u.path == f.path) {
                    f.added = updated.added;
                    f.deleted = updated.deleted;
                    f.conflicts = updated.conflicts;
                } else if self.unchecked.contains(&f.path) {
                    f.conflicts = false;
                }
            }
        }

        self.selected = self
            .selected
            .min(self.known_files.len().saturating_sub(1));
        self.check = Some(check);
        self.sync_list_scroll();
    }

    pub fn selected_file(&self) -> Option<&PatchFileStat> {
        self.known_files.get(self.selected)
    }

    pub fn selected_section(&self) -> Option<&PatchFileSection> {
        let path = &self.selected_file()?.path;
        self.sections
            .iter()
            .find(|s| &s.path == path)
            .or_else(|| self.sections.get(self.selected))
    }

    pub fn preview_line_count(&self) -> usize {
        self.selected_section()
            .map_or(0, |s| s.diff_text.lines().count())
    }

    pub fn sync_list_scroll(&mut self) {
        if let Some(vh) = self.scroll.viewport_height() {
            self.scroll
                .ensure_visible(self.selected, self.known_files.len(), vh);
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.known_files.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != self.selected {
            self.selected = next;
            self.preview_scroll.reset();
            self.sync_list_scroll();
        }
    }

    pub fn select_row(&mut self, idx: usize) {
        let len = self.known_files.len();
        if len == 0 {
            return;
        }
        let next = idx.min(len - 1);
        if next != self.selected {
            self.selected = next;
            self.preview_scroll.reset();
        }
        self.focus = ApplyPatchFocus::Files;
        self.sync_list_scroll();
    }

    pub fn toggle_file_checked(&mut self, idx: Option<usize>) -> bool {
        let idx = idx.unwrap_or(self.selected);
        let Some(path) = self.known_files.get(idx).map(|f| f.path.clone()) else {
            return false;
        };
        if !self.unchecked.remove(&path) {
            self.unchecked.insert(path);
        }
        self.sync_include_paths();
        true
    }

    pub fn toggle_all_checked(&mut self) -> bool {
        if self.known_files.is_empty() {
            return false;
        }
        if self.unchecked.is_empty() {
            // All checked -> uncheck all.
            self.unchecked = self.known_files.iter().map(|f| f.path.clone()).collect();
        } else {
            // Some or all unchecked -> check all.
            self.unchecked.clear();
        }
        self.sync_include_paths();
        true
    }
}

fn top(state: &AppState) -> Option<&ApplyPatchState> {
    match state.modal_stack.last() {
        Some(ModalLayer::ApplyPatch(s)) => Some(s.as_ref()),
        _ => None,
    }
}

/// Actions of the Apply button (`Notice` while disabled).
pub fn confirm_actions(state: &AppState) -> Vec<Action> {
    let Some(s) = top(state) else {
        return Vec::new();
    };
    if s.all_unchecked() {
        return vec![Action::Notice(
            state.i18n.t("dialog.apply_patch.none_selected").to_string(),
        )];
    }
    if !s.can_apply() {
        let key = if s.current_check().is_some() && s.pending_check_due.is_none() {
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
            options: s.options.clone(),
        }),
    ]
}

/// Keyboard routing while the Apply Patch modal is on top.
pub fn route_apply_patch_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(s) = top(state) else {
        return Vec::new();
    };
    if s.editing_dir.is_some() {
        return vec![Action::ApplyPatch(ApplyPatchAction::DirEditKey(*key))];
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if ctrl && !alt {
        return match key.code {
            KeyCode::Enter => confirm_actions(state),
            KeyCode::Char('d') => vec![Action::ApplyPatch(ApplyPatchAction::ScrollPreview(10))],
            KeyCode::Char('u') => vec![Action::ApplyPatch(ApplyPatchAction::ScrollPreview(-10))],
            _ => Vec::new(),
        };
    }
    if alt {
        return Vec::new();
    }
    let ap = Action::ApplyPatch;
    match key.code {
        KeyCode::Esc => vec![ap(ApplyPatchAction::Escape)],
        KeyCode::Char('q') => vec![Action::PopModal],
        KeyCode::Char('A') => confirm_actions(state),
        KeyCode::Char(' ') => vec![ap(ApplyPatchAction::ToggleFile(None))],
        KeyCode::Char('a') => vec![ap(ApplyPatchAction::ToggleAll)],
        KeyCode::Char('d') => vec![ap(ApplyPatchAction::TogglePreview)],
        KeyCode::Enter => {
            if !s.known_files.is_empty() && s.selected_section().is_some() {
                vec![ap(ApplyPatchAction::OpenPreview)]
            } else {
                confirm_actions(state)
            }
        }
        KeyCode::Char('r') => vec![ap(ApplyPatchAction::ToggleReverse)],
        KeyCode::Char('+' | '=') => vec![ap(ApplyPatchAction::AdjustStrip(1))],
        KeyCode::Char('-') => vec![ap(ApplyPatchAction::AdjustStrip(-1))],
        KeyCode::Char('D') => vec![ap(ApplyPatchAction::StartDirEdit)],
        KeyCode::Char('m') => vec![ap(ApplyPatchAction::SetMode(None))],
        KeyCode::Tab | KeyCode::BackTab => {
            if s.show_preview {
                vec![ap(ApplyPatchAction::ToggleFocus)]
            } else if key.code == KeyCode::BackTab {
                let prev = s.options.mode.next().next();
                vec![ap(ApplyPatchAction::SetMode(Some(prev)))]
            } else {
                vec![ap(ApplyPatchAction::SetMode(None))]
            }
        }
        KeyCode::Right => {
            if s.show_preview {
                vec![ap(ApplyPatchAction::ToggleFocus)]
            } else {
                vec![ap(ApplyPatchAction::SetMode(None))]
            }
        }
        KeyCode::Left => {
            if s.show_preview {
                vec![ap(ApplyPatchAction::ToggleFocus)]
            } else {
                let prev = s.options.mode.next().next();
                vec![ap(ApplyPatchAction::SetMode(Some(prev)))]
            }
        }
        KeyCode::Char('1') => vec![ap(ApplyPatchAction::SetMode(Some(ApplyMode::WorkingTree)))],
        KeyCode::Char('2') => vec![ap(ApplyPatchAction::SetMode(Some(ApplyMode::Index)))],
        KeyCode::Char('3') => vec![ap(ApplyPatchAction::SetMode(Some(ApplyMode::ThreeWay)))],
        KeyCode::Down | KeyCode::Char('j') => {
            if s.show_preview && s.focus == ApplyPatchFocus::Preview {
                vec![ap(ApplyPatchAction::ScrollPreview(1))]
            } else {
                vec![ap(ApplyPatchAction::Move(1))]
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if s.show_preview && s.focus == ApplyPatchFocus::Preview {
                vec![ap(ApplyPatchAction::ScrollPreview(-1))]
            } else {
                vec![ap(ApplyPatchAction::Move(-1))]
            }
        }
        KeyCode::PageDown => {
            if s.show_preview && s.focus == ApplyPatchFocus::Preview {
                vec![ap(ApplyPatchAction::ScrollPreview(10))]
            } else {
                vec![ap(ApplyPatchAction::Move(10))]
            }
        }
        KeyCode::PageUp => {
            if s.show_preview && s.focus == ApplyPatchFocus::Preview {
                vec![ap(ApplyPatchAction::ScrollPreview(-10))]
            } else {
                vec![ap(ApplyPatchAction::Move(-10))]
            }
        }
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
    if screen.width < 36 || screen.height < 12 {
        return;
    }
    let check = s.current_check();
    let rows = s.known_files.len().max(1) as u16;
    let w = if s.show_preview {
        screen.width.saturating_sub(4).clamp(50.min(screen.width.saturating_sub(2)), 140)
    } else {
        screen.width.saturating_sub(4).clamp(36.min(screen.width.saturating_sub(2)), 108)
    };
    let h = if s.show_preview {
        screen.height.saturating_sub(2).clamp(16.min(screen.height.saturating_sub(2)), 36)
    } else {
        (rows + 11)
            .clamp(13, 30)
            .min(screen.height.saturating_sub(2))
    };
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

    // Row 0: Source.
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

    // Row 1: Mode radio row.
    let mut x = inner.x + 1;
    let mode_y = inner.y + 1;
    let label = format!("{} ", ctx.t("dialog.apply_patch.mode"));
    let lw = label.width() as u16;
    if mode_y < inner.bottom() {
        frame.render_widget(
            Paragraph::new(Span::styled(label, muted)),
            Rect::new(x, mode_y, lw.min(inner.right().saturating_sub(x)), 1),
        );
        x += lw;
        for (i, mode) in ApplyMode::ALL.into_iter().enumerate() {
            let on = mode == s.options.mode;
            let name = ctx.t(match mode {
                ApplyMode::WorkingTree => "dialog.apply_patch.mode_worktree",
                ApplyMode::Index => "dialog.apply_patch.mode_index",
                ApplyMode::ThreeWay => "dialog.apply_patch.mode_3way",
            });
            let text = format!("{} {}:{name}  ", if on { "●" } else { "○" }, i + 1);
            let tw = text.width() as u16;
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
            let r = Rect::new(x, mode_y, tw, 1);
            frame.render_widget(Paragraph::new(Span::styled(text, style)), r);
            ctx.hit_registry
                .register(r, z + 2, HitTarget::ApplyPatchMode(mode));
            x += tw;
        }
    }

    // Row 2: Depth options (-R reverse, -p<n> strip, --directory=<dir>).
    let opt_y = inner.y + 2;
    if opt_y < inner.bottom() {
        if let Some(draft) = &s.editing_dir {
            let prompt = ctx.t("dialog.apply_patch.dir_prompt");
            let text = format!(" {prompt}: {draft}▏  ({})", ctx.t("dialog.apply_patch.dir_edit_hint"));
            line(
                frame,
                opt_y,
                Line::from(Span::styled(
                    truncate_to_width(&text, width),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .bg(ctx.theme.bg_selection_focused),
                )),
            );
        } else {
            let mut ox = inner.x + 1;
            let rev_box = if s.options.reverse { "[x]" } else { "[ ]" };
            let rev_text = format!(
                "{rev_box} r:{}  ",
                ctx.t("dialog.apply_patch.reverse")
            );
            let rev_w = rev_text.width() as u16;
            if ox + rev_w <= inner.right() {
                let r = Rect::new(ox, opt_y, rev_w, 1);
                let style = if s.options.reverse {
                    Style::default()
                        .fg(ctx.theme.fg_accent_link)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(ctx.theme.fg_primary)
                };
                frame.render_widget(Paragraph::new(Span::styled(rev_text, style)), r);
                ctx.hit_registry
                    .register(r, z + 2, HitTarget::ApplyPatchToggleReverse);
                ox += rev_w;
            }

            let strip_val = s.options.effective_strip();
            let strip_lbl = format!("-p{strip_val} ");
            let strip_lw = strip_lbl.width() as u16;
            if ox + strip_lw + 7 <= inner.right() {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        strip_lbl,
                        Style::default().fg(ctx.theme.fg_primary),
                    )),
                    Rect::new(ox, opt_y, strip_lw, 1),
                );
                ox += strip_lw;
                let dec_r = Rect::new(ox, opt_y, 3, 1);
                frame.render_widget(Paragraph::new(Span::styled("[-]", muted)), dec_r);
                ctx.hit_registry
                    .register(dec_r, z + 2, HitTarget::ApplyPatchStrip(-1));
                ox += 3;
                let inc_r = Rect::new(ox, opt_y, 3, 1);
                frame.render_widget(Paragraph::new(Span::styled("[+]", muted)), inc_r);
                ctx.hit_registry
                    .register(inc_r, z + 2, HitTarget::ApplyPatchStrip(1));
                ox += 5;
            }

            let dir_val = s
                .options
                .normalized_directory()
                .unwrap_or_else(|| ctx.t("dialog.apply_patch.dir_root").to_string());
            let dir_text = format!("D:{}: {dir_val}", ctx.t("dialog.apply_patch.directory"));
            let avail = inner.right().saturating_sub(ox);
            if avail > 4 {
                let dir_r = Rect::new(ox, opt_y, avail, 1);
                let style = if s.options.normalized_directory().is_some() {
                    Style::default()
                        .fg(ctx.theme.fg_accent_link)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(ctx.theme.fg_primary)
                };
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        truncate_to_width(&dir_text, avail as usize),
                        style,
                    )),
                    dir_r,
                );
                ctx.hit_registry
                    .register(dir_r, z + 2, HitTarget::ApplyPatchEditDir);
            }
        }
    }

    // Body region: list + optional error banner + optional right diff preview.
    let body_top = inner.y + 4;
    let footer_y = inner.bottom().saturating_sub(2);
    let body_h = footer_y.saturating_sub(body_top);

    let (left_area, right_area) = if s.show_preview && inner.width >= 48 {
        let lw = (inner.width * 2 / 5).clamp(20, 52);
        let sep_x = inner.x + lw;
        for y in body_top..body_top + body_h {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "│",
                    Style::default().fg(ctx.theme.border_splitter),
                )),
                Rect::new(sep_x, y, 1, 1),
            );
        }
        (
            Rect::new(inner.x, body_top, lw, body_h),
            Some(Rect::new(
                sep_x + 1,
                body_top,
                inner.right().saturating_sub(sep_x + 1),
                body_h,
            )),
        )
    } else {
        (Rect::new(inner.x, body_top, inner.width, body_h), None)
    };

    render_file_list_pane(frame, left_area, s, check, ctx, z);
    if let Some(preview_rect) = right_area {
        render_patch_preview_pane(frame, preview_rect, s, ctx, z);
    }

    // Summary + buttons.
    let summary = if s.all_unchecked() {
        ctx.t("dialog.apply_patch.none_selected").to_string()
    } else if s.pending_check_due.is_some() || check.is_none() {
        ctx.t("dialog.apply_patch.checking").to_string()
    } else if let Some(c) = check {
        if c.applies() {
            let checked = s.checked_count().to_string();
            let total = s.known_files.len().max(c.files.len()).to_string();
            let conflicts = c.conflict_count();
            if conflicts > 0 {
                ctx.t_fmt(
                    "dialog.apply_patch.summary_selected_conflicts",
                    &[
                        ("checked", &checked),
                        ("total", &total),
                        ("conflicts", &conflicts.to_string()),
                    ],
                )
            } else {
                ctx.t_fmt(
                    "dialog.apply_patch.summary_selected",
                    &[("checked", &checked), ("total", &total)],
                )
            }
        } else {
            String::new()
        }
    } else {
        String::new()
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
    let aw = apply.width() as u16;
    let cw = cancel.width() as u16;
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

fn render_file_list_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    s: &ApplyPatchState,
    check: Option<&PatchCheck>,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let width = area.width as usize;
    let mut y = area.y;

    // Show error + fallback guidance at top of left pane when check failed.
    if let Some(c) = check
        && !c.applies()
    {
        let err = c.error.clone().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" ✗ {err}"), width),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
        if y < area.bottom() {
            if c.needs_three_way_index_fallback() {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        truncate_to_width(
                            &format!(" {}", ctx.t("dialog.apply_patch.three_way_no_index")),
                            width,
                        ),
                        Style::default().fg(ctx.theme.status_modified),
                    )),
                    Rect::new(area.x, y, area.width, 1),
                );
                y += 1;
            } else if s.options.mode != ApplyMode::ThreeWay {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        truncate_to_width(
                            &format!(" {}", ctx.t("dialog.apply_patch.try_3way")),
                            width,
                        ),
                        muted,
                    )),
                    Rect::new(area.x, y, area.width, 1),
                );
                y += 1;
            }
        }
    }

    let list_h = area.bottom().saturating_sub(y) as usize;
    if list_h == 0 {
        ctx.record_scroll_bound(ScrollPane::ApplyPatch, 0, 0);
        return;
    }

    if s.known_files.is_empty() {
        ctx.record_scroll_bound(ScrollPane::ApplyPatch, list_h, 0);
        if check.is_none() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(
                        &format!(" {}", ctx.t("dialog.apply_patch.checking")),
                        width,
                    ),
                    muted,
                )),
                Rect::new(area.x, y, area.width, 1),
            );
        }
        return;
    }

    let max_scroll = s.known_files.len().saturating_sub(list_h.max(1));
    let mut vp = s.scroll;
    vp.ensure_visible(s.selected, s.known_files.len(), list_h);
    let first = vp.offset().min(max_scroll);
    ctx.record_scroll_bound(ScrollPane::ApplyPatch, list_h, max_scroll);

    let focused = !s.show_preview || s.focus == ApplyPatchFocus::Files;
    for (row, (idx, f)) in s
        .known_files
        .iter()
        .enumerate()
        .skip(first)
        .take(list_h)
        .enumerate()
    {
        let row_y = y + row as u16;
        let is_sel = idx == s.selected;
        let is_checked = s.is_checked(&f.path);
        let bg = if is_sel {
            if focused {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_selection_unfocused
            }
        } else {
            ctx.theme.bg_elevated
        };
        let fg = if is_checked {
            ctx.theme.fg_primary
        } else {
            ctx.theme.fg_muted
        };
        let check_mark = if is_checked { "[x]" } else { "[ ]" };
        let path_w = width.saturating_sub(20).max(8);
        let mut spans = vec![
            Span::styled(
                format!(" {check_mark} "),
                Style::default()
                    .fg(if is_checked {
                        ctx.theme.fg_accent_link
                    } else {
                        ctx.theme.fg_muted
                    })
                    .bg(bg),
            ),
            Span::styled(
                format!("{} ", truncate_to_width(&f.path, path_w)),
                Style::default().fg(fg).bg(bg),
            ),
        ];
        if let (Some(a), Some(d)) = (f.added, f.deleted) {
            spans.push(Span::styled(
                format!("+{a}"),
                Style::default().fg(ctx.theme.status_added).bg(bg),
            ));
            spans.push(Span::styled(" ", muted.bg(bg)));
            spans.push(Span::styled(
                format!("-{d}"),
                Style::default().fg(ctx.theme.status_deleted).bg(bg),
            ));
        } else if check.is_some_and(PatchCheck::applies) && is_checked {
            spans.push(Span::styled(
                ctx.t("dialog.apply_patch.binary").to_string(),
                muted.bg(bg),
            ));
        }
        if f.conflicts && is_checked {
            spans.push(Span::styled(
                format!(" {}", ctx.t("dialog.apply_patch.will_conflict")),
                Style::default().fg(ctx.theme.status_conflicted).bg(bg),
            ));
        }

        let row_rect = Rect::new(area.x, row_y, area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        // Checkbox hit target on the first 5 columns, row select on the remainder.
        let cb_w = 5u16.min(area.width);
        ctx.hit_registry.register(
            Rect::new(area.x, row_y, cb_w, 1),
            z + 3,
            HitTarget::ApplyPatchFileCheck(idx),
        );
        ctx.hit_registry
            .register(row_rect, z + 2, HitTarget::ApplyPatchFileRow(idx));
    }
}

fn render_patch_preview_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    s: &ApplyPatchState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    ctx.hit_registry
        .register(area, z + 2, HitTarget::ApplyPatchPreview);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let width = area.width as usize;

    let Some(section) = s.selected_section() else {
        ctx.record_scroll_bound(ScrollPane::ApplyPatchPreview, area.height as usize, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(
                    &format!(" {}", ctx.t("dialog.apply_patch.no_preview")),
                    width,
                ),
                muted,
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };

    // Header row inside preview pane.
    let hdr_bg = if s.focus == ApplyPatchFocus::Preview {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_panel
    };
    let hdr = format!(" {} ", section.path);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hdr, width),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(hdr_bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(hdr_bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let body_h = area.height.saturating_sub(1) as usize;
    if body_h == 0 {
        ctx.record_scroll_bound(ScrollPane::ApplyPatchPreview, 0, 0);
        return;
    }

    let lines: Vec<&str> = section.diff_text.lines().collect();
    let max_scroll = lines.len().saturating_sub(body_h.max(1));
    ctx.record_scroll_bound(ScrollPane::ApplyPatchPreview, body_h, max_scroll);
    let first = s.preview_scroll.offset().min(max_scroll);
    let syntax_on = ctx
        .config
        .is_none_or(|c| c.jbgit.diff.syntax_highlight);
    let true_color = ctx.theme.true_color;

    let tab_size = ctx.diff_tab_size();
    for (v, raw) in lines.iter().skip(first).take(body_h).enumerate() {
        let row_y = area.y + 1 + v as u16;
        let row_rect = Rect::new(area.x, row_y, area.width, 1);

        let (bg, default_fg, code_body) = if raw.starts_with("@@") {
            (ctx.theme.bg_panel, ctx.theme.fg_accent_link, None)
        } else if raw.starts_with("+++")
            || raw.starts_with("---")
            || raw.starts_with("diff --git")
            || raw.starts_with("index ")
        {
            (ctx.theme.bg_elevated, ctx.theme.fg_muted, None)
        } else if let Some(rest) = raw.strip_prefix('+') {
            (ctx.theme.diff_add_bg, ctx.theme.status_added, Some(('+', rest)))
        } else if let Some(rest) = raw.strip_prefix('-') {
            (ctx.theme.diff_del_bg, ctx.theme.status_deleted, Some(('-', rest)))
        } else if let Some(rest) = raw.strip_prefix(' ') {
            (ctx.theme.bg_elevated, ctx.theme.fg_primary, Some((' ', rest)))
        } else {
            (ctx.theme.bg_elevated, ctx.theme.fg_muted, None)
        };

        let spans: Vec<Span<'static>> = match code_body {
            Some((prefix, body)) => {
                let mut out = vec![Span::styled(
                    format!(" {prefix}"),
                    Style::default().fg(default_fg).bg(bg),
                )];
                let tokens = if syntax_on {
                    highlight_single_line(&section.path, body)
                } else {
                    Vec::new()
                };
                out.extend(render_diff_line_spans_with_tab(
                    body,
                    &tokens,
                    &[],
                    width.saturating_sub(2),
                    tab_size,
                    Style::default().fg(ctx.theme.fg_primary).bg(bg),
                    bg,
                    true_color,
                ));
                out
            }
            None => vec![Span::styled(
                truncate_to_width(&format!(" {raw}"), width),
                Style::default().fg(default_fg).bg(bg),
            )],
        };

        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
    }
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
