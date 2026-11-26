//! Repository lifecycle modals & dialogs (Spec 13 Issue 13):
//! - [`WelcomeState`]: non-Git directory startup welcome screen (`Init` / `Clone…` / `Open Recent`).
//! - [`CloneDialogState`]: `git clone` form with URL-inferred target directory,
//!   optional `--depth` / `--recurse-submodules`, live progress streaming, and
//!   Askpass credential integration.
//! - [`crlf_warning_dialog`]: Windows CRLF pre-commit guard with 3-way resolution
//!   (`Fix & Commit` / `Commit As Is` / `Don't warn again`).

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_engine::CommitRequest;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext};
use crate::dialogs::{ChoiceDialog, ChoiceOption, Dialog, resolve_path};
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// Number of entries in the Welcome modal.
pub const WELCOME_ENTRY_COUNT: usize = 3;

/// Self-contained state of [`ModalLayer::Welcome`] shown when `jbgit` starts in
/// a directory that is not inside a Git repository.
#[derive(Debug, Clone)]
pub struct WelcomeState {
    pub dir: PathBuf,
    pub selected: usize,
    pub initializing: bool,
    pub error: Option<String>,
}

impl WelcomeState {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            selected: 0,
            initializing: false,
            error: None,
        }
    }

    pub fn activate_index(&self, idx: usize) -> Vec<Action> {
        if self.initializing {
            return Vec::new();
        }
        match idx {
            0 => vec![Action::WelcomeInitRepo(self.dir.clone())],
            1 => vec![Action::OpenCloneRepo {
                base_dir: Some(self.dir.clone()),
            }],
            2 => vec![Action::OpenRepoSwitcher],
            _ => Vec::new(),
        }
    }
}

/// Focused field in [`CloneDialogState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneField {
    Url,
    TargetDir,
    Depth,
    RecurseSubmodules,
}

impl CloneField {
    pub const ALL: [Self; 4] = [
        Self::Url,
        Self::TargetDir,
        Self::Depth,
        Self::RecurseSubmodules,
    ];

    pub const fn next(self) -> Self {
        match self {
            Self::Url => Self::TargetDir,
            Self::TargetDir => Self::Depth,
            Self::Depth => Self::RecurseSubmodules,
            Self::RecurseSubmodules => Self::Url,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Url => Self::RecurseSubmodules,
            Self::TargetDir => Self::Url,
            Self::Depth => Self::TargetDir,
            Self::RecurseSubmodules => Self::Depth,
        }
    }

    pub fn step_clamped(self, delta: isize) -> Self {
        let cur = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        Self::ALL[crate::nav::step_index(cur, Self::ALL.len(), delta, false)]
    }
}

/// Mutations applied to the active [`ModalLayer::CloneRepo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneDialogAction {
    SetFocus(CloneField),
    NextField,
    PrevField,
    MoveField(isize),
    SelectAll,
    DeselectAll,
    InsertChar(char),
    Paste(String),
    Backspace,
    Delete,
    ClearField,
    ToggleSubmodules,
}

/// Self-contained state of [`ModalLayer::CloneRepo`].
#[derive(Debug, Clone)]
pub struct CloneDialogState {
    pub base_dir: PathBuf,
    pub url: String,
    pub target_dir: String,
    /// `true` once the user manually edits `target_dir` so URL changes stop
    /// overwriting it.
    pub dir_edited: bool,
    pub depth: String,
    pub recurse_submodules: bool,
    pub focus: CloneField,
    pub selected_all: bool,
    pub error: Option<String>,
    pub cloning: bool,
    pub progress_lines: Vec<String>,
}

impl CloneDialogState {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir: jbgit_engine::probe::strip_verbatim_prefix(base_dir),
            url: String::new(),
            target_dir: String::new(),
            dir_edited: false,
            depth: String::new(),
            recurse_submodules: false,
            focus: CloneField::Url,
            selected_all: false,
            error: None,
            cloning: false,
            progress_lines: Vec::new(),
        }
    }

    /// Re-derives `target_dir` from `url` unless the user has manually edited
    /// the target directory field.
    pub fn sync_inferred_target_dir(&mut self) {
        if self.dir_edited {
            return;
        }
        match jbgit_core::infer_clone_dir_name(&self.url) {
            Some(name) => {
                let base_dir = jbgit_engine::probe::strip_verbatim_prefix(self.base_dir.clone());
                let path = if base_dir.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    base_dir.join(name)
                };
                self.target_dir = jbgit_engine::probe::strip_verbatim_prefix(path)
                    .display()
                    .to_string();
            }
            None if self.url.trim().is_empty() => {
                self.target_dir.clear();
            }
            None => {}
        }
    }

    pub fn apply(&mut self, act: CloneDialogAction) {
        if self.cloning {
            return;
        }
        self.error = None;
        match act {
            CloneDialogAction::SetFocus(field) => {
                self.focus = field;
                self.selected_all = false;
            }
            CloneDialogAction::NextField => {
                self.focus = self.focus.next();
                self.selected_all = false;
            }
            CloneDialogAction::PrevField => {
                self.focus = self.focus.prev();
                self.selected_all = false;
            }
            CloneDialogAction::MoveField(delta) => {
                self.focus = self.focus.step_clamped(delta);
                self.selected_all = false;
            }
            CloneDialogAction::SelectAll => {
                self.selected_all = match self.focus {
                    CloneField::Url => !self.url.is_empty(),
                    CloneField::TargetDir => !self.target_dir.is_empty(),
                    CloneField::Depth => !self.depth.is_empty(),
                    CloneField::RecurseSubmodules => false,
                };
            }
            CloneDialogAction::DeselectAll => {
                self.selected_all = false;
            }
            CloneDialogAction::InsertChar(c) => {
                if c.is_control() {
                    return;
                }
                match self.focus {
                    CloneField::Url => {
                        if self.selected_all {
                            self.url.clear();
                            self.selected_all = false;
                        }
                        self.url.push(c);
                        self.sync_inferred_target_dir();
                    }
                    CloneField::TargetDir => {
                        if self.selected_all {
                            self.target_dir.clear();
                            self.selected_all = false;
                        }
                        self.dir_edited = true;
                        self.target_dir.push(c);
                    }
                    CloneField::Depth => {
                        if c.is_ascii_digit() {
                            if self.selected_all {
                                self.depth.clear();
                                self.selected_all = false;
                            }
                            self.depth.push(c);
                        }
                    }
                    CloneField::RecurseSubmodules => {
                        if c == ' ' {
                            self.recurse_submodules = !self.recurse_submodules;
                        }
                    }
                }
            }
            CloneDialogAction::Paste(text) => {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                if clean.is_empty() {
                    return;
                }
                match self.focus {
                    CloneField::Url => {
                        if self.selected_all {
                            self.url.clear();
                            self.selected_all = false;
                        }
                        self.url.push_str(clean.trim());
                        self.sync_inferred_target_dir();
                    }
                    CloneField::TargetDir => {
                        if self.selected_all {
                            self.target_dir.clear();
                            self.selected_all = false;
                        }
                        self.dir_edited = true;
                        self.target_dir.push_str(&clean);
                    }
                    CloneField::Depth => {
                        let digits: String = clean.chars().filter(char::is_ascii_digit).collect();
                        if !digits.is_empty() {
                            if self.selected_all {
                                self.depth.clear();
                                self.selected_all = false;
                            }
                            self.depth.push_str(&digits);
                        }
                    }
                    CloneField::RecurseSubmodules => {}
                }
            }
            CloneDialogAction::Backspace => match self.focus {
                CloneField::Url => {
                    if self.selected_all {
                        self.url.clear();
                        self.selected_all = false;
                    } else {
                        self.url.pop();
                    }
                    self.sync_inferred_target_dir();
                }
                CloneField::TargetDir => {
                    self.dir_edited = true;
                    if self.selected_all {
                        self.target_dir.clear();
                        self.selected_all = false;
                    } else {
                        self.target_dir.pop();
                    }
                }
                CloneField::Depth => {
                    if self.selected_all {
                        self.depth.clear();
                        self.selected_all = false;
                    } else {
                        self.depth.pop();
                    }
                }
                CloneField::RecurseSubmodules => {}
            },
            CloneDialogAction::Delete => {
                if self.selected_all {
                    self.selected_all = false;
                    match self.focus {
                        CloneField::Url => {
                            self.url.clear();
                            self.sync_inferred_target_dir();
                        }
                        CloneField::TargetDir => {
                            self.dir_edited = true;
                            self.target_dir.clear();
                        }
                        CloneField::Depth => {
                            self.depth.clear();
                        }
                        CloneField::RecurseSubmodules => {}
                    }
                }
            }
            CloneDialogAction::ClearField => {
                self.selected_all = false;
                match self.focus {
                    CloneField::Url => {
                        self.url.clear();
                        self.sync_inferred_target_dir();
                    }
                    CloneField::TargetDir => {
                        self.dir_edited = true;
                        self.target_dir.clear();
                    }
                    CloneField::Depth => {
                        self.depth.clear();
                    }
                    CloneField::RecurseSubmodules => {}
                }
            }
            CloneDialogAction::ToggleSubmodules => {
                self.selected_all = false;
                self.recurse_submodules = !self.recurse_submodules;
            }
        }
    }

    pub fn push_progress(&mut self, line: String) {
        let replace_last = line.starts_with('\r');
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if replace_last && let Some(last) = self.progress_lines.last_mut() {
            *last = trimmed;
            return;
        }
        self.progress_lines.push(trimmed);
        const MAX_LINES: usize = 64;
        if self.progress_lines.len() > MAX_LINES {
            let drain = self.progress_lines.len() - MAX_LINES;
            self.progress_lines.drain(..drain);
        }
    }

    /// Validates inputs and builds a ready-to-run [`jbgit_engine::CloneRequest`].
    pub fn build_request(&self, i18n: &I18n) -> Result<jbgit_engine::CloneRequest, String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(i18n.t("dialog.remotes.url_empty").to_string());
        }
        if url.chars().any(char::is_whitespace) {
            return Err(i18n.t("dialog.remotes.url_spaces").to_string());
        }

        let base_dir = jbgit_engine::probe::strip_verbatim_prefix(self.base_dir.clone());
        let raw_dir = self.target_dir.trim();
        let target_dir = if !raw_dir.is_empty() {
            let cleaned_raw = jbgit_engine::probe::strip_verbatim_prefix_str(raw_dir);
            resolve_path(&base_dir, cleaned_raw.as_deref().unwrap_or(raw_dir))
        } else if let Some(inferred) = jbgit_core::infer_clone_dir_name(url) {
            resolve_path(&base_dir, &inferred)
        } else {
            return Err(i18n.t("dialog.clone.dir_empty").to_string());
        };
        let target_dir = jbgit_engine::probe::strip_verbatim_prefix(target_dir);

        let occupied = target_dir.is_file()
            || std::fs::read_dir(&target_dir).is_ok_and(|mut d| d.next().is_some());
        if occupied {
            return Err(i18n.t("dialog.clone.dir_not_empty").to_string());
        }

        let depth = if self.depth.trim().is_empty() {
            None
        } else {
            match self.depth.trim().parse::<u32>() {
                Ok(n) if n > 0 => Some(n),
                _ => return Err(i18n.t("dialog.clone.invalid_depth").to_string()),
            }
        };

        Ok(jbgit_engine::CloneRequest {
            url: url.to_string(),
            target_dir,
            depth,
            recurse_submodules: self.recurse_submodules,
        })
    }
}

/// Default base directory for [`CloneDialogState`]: the explicit directory if
/// provided, else the current repository's parent directory (when bound), else
/// the displayed startup directory.
pub fn default_clone_base_dir(state: &AppState, explicit: Option<&Path>) -> PathBuf {
    let dir = if let Some(dir) = explicit {
        dir.to_path_buf()
    } else if let Some(snap) = state.repo_snapshot.as_ref() {
        snap.workdir.parent().unwrap_or(&snap.workdir).to_path_buf()
    } else if state.repo_path_display.is_dir() {
        state.repo_path_display.clone()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    jbgit_engine::probe::strip_verbatim_prefix(dir)
}

/// Builds the 3-choice CRLF pre-commit warning dialog (`Fix & Commit` /
/// `Commit As Is` / `Don't warn again`).
pub fn crlf_warning_dialog(i18n: &I18n, request: CommitRequest, files: &[String]) -> Dialog {
    let count_s = files.len().to_string();
    let mut body = vec![
        i18n.t_fmt("dialog.crlf.body", &[("count", &count_s)]),
        i18n.t("dialog.crlf.guidance").to_string(),
    ];
    const MAX_SHOWN: usize = 8;
    for path in files.iter().take(MAX_SHOWN) {
        body.push(format!("  • {path}"));
    }
    if files.len() > MAX_SHOWN {
        let more_s = (files.len() - MAX_SHOWN).to_string();
        body.push(i18n.t_fmt("dialog.crlf.more_files", &[("count", &more_s)]));
    }

    Dialog::Choice(ChoiceDialog::buttons(
        i18n.t("dialog.crlf.title"),
        body,
        vec![
            ChoiceOption::new(
                i18n.t("dialog.crlf.fix_and_commit"),
                Some('f'),
                vec![Action::CommitCrlfResolve {
                    request: request.clone(),
                    fix_autocrlf: true,
                    disable_warning: false,
                }],
            ),
            ChoiceOption::new(
                i18n.t("dialog.crlf.commit_as_is"),
                Some('c'),
                vec![Action::CommitCrlfResolve {
                    request: request.clone(),
                    fix_autocrlf: false,
                    disable_warning: false,
                }],
            ),
            ChoiceOption::new(
                i18n.t("dialog.crlf.dont_warn"),
                Some('d'),
                vec![Action::CommitCrlfResolve {
                    request,
                    fix_autocrlf: false,
                    disable_warning: true,
                }],
            ),
        ],
    ))
}

/// Keyboard routing when [`ModalLayer::Welcome`] is topmost.
pub fn route_welcome_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(ModalLayer::Welcome(ws)) = state.modal_stack.last() else {
        return Vec::new();
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if ctrl || alt {
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            if state.repo_snapshot.is_none() && state.modal_stack.len() == 1 {
                vec![Action::Quit]
            } else {
                vec![Action::PopModal]
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            vec![Action::WelcomeSelect(ws.selected.saturating_sub(1))]
        }
        KeyCode::Down | KeyCode::Char('j') => {
            vec![Action::WelcomeSelect(
                (ws.selected + 1).min(WELCOME_ENTRY_COUNT - 1),
            )]
        }
        KeyCode::Enter | KeyCode::Char(' ') => ws.activate_index(ws.selected),
        KeyCode::Char('i') | KeyCode::Char('I') => ws.activate_index(0),
        KeyCode::Char('c') | KeyCode::Char('C') => ws.activate_index(1),
        KeyCode::Char('o') | KeyCode::Char('O') => ws.activate_index(2),
        _ => Vec::new(),
    }
}

/// Keyboard routing when [`ModalLayer::CloneRepo`] is topmost.
pub fn route_clone_repo_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    let Some(ModalLayer::CloneRepo(cd)) = state.modal_stack.last() else {
        return Vec::new();
    };
    if cd.cloning {
        return match key.code {
            KeyCode::Esc => vec![Action::CloneRepoCancel],
            _ => Vec::new(),
        };
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => vec![Action::PopModal],
        KeyCode::Tab => vec![Action::CloneRepoUpdate(CloneDialogAction::NextField)],
        KeyCode::BackTab => vec![Action::CloneRepoUpdate(CloneDialogAction::PrevField)],
        KeyCode::Down => vec![Action::CloneRepoUpdate(CloneDialogAction::MoveField(1))],
        KeyCode::Up => vec![Action::CloneRepoUpdate(CloneDialogAction::MoveField(-1))],
        KeyCode::Enter => vec![Action::CloneRepoSubmit],
        KeyCode::Char('a' | 'A') if ctrl && !alt => {
            vec![Action::CloneRepoUpdate(CloneDialogAction::SelectAll)]
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End if cd.selected_all => {
            vec![Action::CloneRepoUpdate(CloneDialogAction::DeselectAll)]
        }
        KeyCode::Char('u' | 'U') if ctrl && !alt => {
            vec![Action::CloneRepoUpdate(CloneDialogAction::ClearField)]
        }
        KeyCode::Backspace => vec![Action::CloneRepoUpdate(CloneDialogAction::Backspace)],
        KeyCode::Delete => vec![Action::CloneRepoUpdate(CloneDialogAction::Delete)],
        KeyCode::Char(' ') if cd.focus == CloneField::RecurseSubmodules && !ctrl && !alt => {
            vec![Action::CloneRepoUpdate(CloneDialogAction::ToggleSubmodules)]
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            vec![Action::CloneRepoUpdate(CloneDialogAction::InsertChar(c))]
        }
        _ => Vec::new(),
    }
}

/// Renders [`ModalLayer::Welcome`] centered over the terminal canvas.
pub fn render_welcome_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ws: &WelcomeState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 28 || screen.height < 10 {
        return;
    }
    let git_summary = state.git_executable_summary();
    let extra_rows =
        u16::from(git_summary.is_some()) + u16::from(ws.initializing || ws.error.is_some());
    let w = screen
        .width
        .saturating_sub(4)
        .clamp(28.min(screen.width), 78);
    let h = (11 + extra_rows).min(screen.height.saturating_sub(2).max(10));
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    );
    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let title = format!(" {} ", ctx.t("view.welcome.title"));
    let block = Block::default()
        .title(Span::styled(
            title,
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
    if inner.height < 7 || inner.width < 12 {
        return;
    }
    let width = inner.width as usize;
    let mut y = inner.y;

    let dir_str = ws.dir.display().to_string();
    let subtitle = ctx.t_fmt("view.welcome.not_a_repo", &[("path", &dir_str)]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {subtitle}"), width),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    if let Some((summary, is_err)) = git_summary
        && y + 5 < inner.bottom()
    {
        let style = if is_err {
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ctx.theme.fg_muted)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {summary}"), width),
                style,
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }
    y += 1; // blank spacer

    let entries = [
        (
            "i",
            ctx.t("view.welcome.init_title").to_string(),
            ctx.t("view.welcome.init_desc").to_string(),
        ),
        (
            "c",
            ctx.t("view.welcome.clone_title").to_string(),
            ctx.t("view.welcome.clone_desc").to_string(),
        ),
        (
            "o",
            ctx.t("view.welcome.open_title").to_string(),
            ctx.t("view.welcome.open_desc").to_string(),
        ),
    ];

    for (idx, (hotkey, label, desc)) in entries.iter().enumerate() {
        if y + 1 >= inner.bottom() {
            break;
        }
        let is_sel = idx == ws.selected;
        let bg = if is_sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let prefix = if is_sel { " ▸ " } else { "   " };
        let badge = format!("[{hotkey}] ");
        let body = format!("{label} — {desc}");
        let budget = width.saturating_sub(prefix.width() + badge.width() + 1);
        let line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(ctx.theme.fg_accent_link)),
            Span::styled(
                badge,
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_to_width(&body, budget),
                Style::default()
                    .fg(if is_sel {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    })
                    .add_modifier(if is_sel {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]);
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 2, HitTarget::WelcomeItem(idx));
        y += 1;
    }

    if ws.initializing && y + 1 < inner.bottom() {
        y += 1;
        let sp = state.spinner_char().to_string();
        let msg = ctx.t_fmt("view.welcome.initializing", &[("spinner", &sp)]);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {msg}"), width),
                Style::default().fg(ctx.theme.border_focused),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
    } else if let Some(err) = &ws.error
        && y + 1 < inner.bottom()
    {
        y += 1;
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {err}"), width),
                Style::default()
                    .fg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    let hint = ctx.t("view.welcome.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), width),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

/// Renders [`ModalLayer::CloneRepo`] centered over the terminal canvas.
pub fn render_clone_repo_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    cd: &CloneDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 32 || screen.height < 12 {
        return;
    }
    let show_progress = cd.cloning || !cd.progress_lines.is_empty();
    let err_line_count = cd
        .error
        .as_deref()
        .map(|e| e.lines().filter(|l| !l.trim().is_empty()).count().min(4))
        .unwrap_or(0);
    let desired_h: u16 = if show_progress {
        20
    } else {
        15 + err_line_count.saturating_sub(1) as u16
    };
    let w = screen
        .width
        .saturating_sub(4)
        .clamp(32.min(screen.width), 86);
    let h = desired_h.min(screen.height.saturating_sub(2).max(12));
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    );
    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let title = format!(" {} ", ctx.t("dialog.clone.title"));
    let block = Block::default()
        .title(Span::styled(
            title,
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
    if inner.height < 8 || inner.width < 16 {
        return;
    }

    let width = inner.width as usize;
    let mut y = inner.y;

    let render_text_field = |frame: &mut Frame<'_>,
                             ctx: &mut RenderContext<'_>,
                             y: &mut u16,
                             label: &str,
                             value: &str,
                             placeholder: &str,
                             field: CloneField| {
        if *y + 2 >= inner.bottom() {
            return;
        }
        let focused = !cd.cloning && cd.focus == field;
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&format!(" {label}"), width),
                Style::default()
                    .fg(if focused {
                        ctx.theme.fg_accent_link
                    } else {
                        ctx.theme.fg_muted
                    })
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )),
            Rect::new(inner.x, *y, inner.width, 1),
        );
        *y += 1;

        let input_bg = if focused {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_panel
        };
        let input_budget = width.saturating_sub(3);
        let line = if value.is_empty() && !focused {
            Line::from(Span::styled(
                format!(" {}", truncate_to_width(placeholder, input_budget)),
                Style::default().fg(ctx.theme.fg_muted),
            ))
        } else {
            let shown_val = truncate_to_width(value, input_budget);
            let mut spans = if focused && cd.selected_all && !shown_val.is_empty() {
                vec![
                    Span::styled(" ", Style::default().fg(ctx.theme.fg_bright)),
                    Span::styled(
                        shown_val,
                        Style::default()
                            .fg(ctx.theme.fg_bright)
                            .bg(ctx.theme.border_focused),
                    ),
                ]
            } else {
                vec![Span::styled(
                    format!(" {shown_val}"),
                    Style::default().fg(ctx.theme.fg_bright),
                )]
            };
            if focused {
                spans.push(Span::styled(
                    "▏",
                    Style::default().fg(ctx.theme.fg_accent_link),
                ));
            }
            Line::from(spans)
        };
        let row_rect = Rect::new(inner.x, *y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(input_bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 2, HitTarget::CloneField(field));
        *y += 1;
    };

    let url_label = ctx.t("dialog.clone.url_label").to_string();
    let url_ph = ctx.t("dialog.clone.url_placeholder").to_string();
    render_text_field(
        frame,
        ctx,
        &mut y,
        &url_label,
        &cd.url,
        &url_ph,
        CloneField::Url,
    );

    let dir_label = ctx.t("dialog.clone.dir_label").to_string();
    let dir_ph = ctx.t("dialog.clone.dir_placeholder").to_string();
    render_text_field(
        frame,
        ctx,
        &mut y,
        &dir_label,
        &cd.target_dir,
        &dir_ph,
        CloneField::TargetDir,
    );

    let depth_label = ctx.t("dialog.clone.depth_label").to_string();
    let depth_ph = ctx.t("dialog.clone.depth_placeholder").to_string();
    render_text_field(
        frame,
        ctx,
        &mut y,
        &depth_label,
        &cd.depth,
        &depth_ph,
        CloneField::Depth,
    );

    if y + 2 < inner.bottom() {
        let focused = !cd.cloning && cd.focus == CloneField::RecurseSubmodules;
        let check = if cd.recurse_submodules { "[x]" } else { "[ ]" };
        let label = ctx.t("dialog.clone.submodules_label").to_string();
        let bg = if focused {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {check} "),
                    Style::default()
                        .fg(ctx.theme.fg_accent_link)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_to_width(&label, width.saturating_sub(6)),
                    Style::default().fg(if focused {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    }),
                ),
            ]))
            .style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            z + 2,
            HitTarget::CloneField(CloneField::RecurseSubmodules),
        );
        y += 1;
    }

    if let Some(err) = &cd.error
        && y + 2 < inner.bottom()
    {
        let err_lines: Vec<&str> = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let max_err_rows = (inner.bottom().saturating_sub(y + 2) as usize).min(4);
        let skip = err_lines.len().saturating_sub(max_err_rows);
        for line in err_lines.into_iter().skip(skip).take(max_err_rows) {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&format!(" {line}"), width),
                    Style::default()
                        .fg(ctx.theme.status_conflicted)
                        .add_modifier(Modifier::BOLD),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
    }

    if show_progress && y + 2 < inner.bottom() {
        if cd.cloning {
            let sp = state.spinner_char().to_string();
            let status = ctx.t_fmt("dialog.clone.cloning", &[("spinner", &sp)]);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&format!(" {status}"), width),
                    Style::default()
                        .fg(ctx.theme.border_focused)
                        .add_modifier(Modifier::BOLD),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
        let avail = inner.bottom().saturating_sub(y + 2) as usize;
        let skip = cd.progress_lines.len().saturating_sub(avail);
        for line in cd.progress_lines.iter().skip(skip).take(avail) {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    truncate_to_width(&format!("  {line}"), width),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
    }

    // Buttons row right above footer hint
    let btn_y = inner.bottom().saturating_sub(2);
    if btn_y >= y {
        let submit_lbl = format!(" [ {} ] ", ctx.t("dialog.clone.confirm_btn"));
        let cancel_lbl = format!(" [ {} ] ", ctx.cancel());
        let submit_w = submit_lbl.width() as u16;
        let cancel_w = cancel_lbl.width() as u16;
        let submit_rect = Rect::new(inner.x + 1, btn_y, submit_w.min(inner.width), 1);
        let cancel_x = inner.x + 1 + submit_w + 1;
        let cancel_rect = Rect::new(
            cancel_x.min(inner.right()),
            btn_y,
            cancel_w.min(inner.right().saturating_sub(cancel_x)),
            1,
        );
        let submit_style = if cd.cloning {
            Style::default().fg(ctx.theme.fg_muted)
        } else {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_focused)
                .add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(submit_lbl, submit_style)),
            submit_rect,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                cancel_lbl,
                Style::default().fg(ctx.theme.fg_primary),
            )),
            cancel_rect,
        );
        if !cd.cloning {
            ctx.hit_registry
                .register(submit_rect, z + 2, HitTarget::CloneButton { submit: true });
            ctx.hit_registry
                .register(cancel_rect, z + 2, HitTarget::CloneButton { submit: false });
        }
    }

    let hint = ctx.t("dialog.clone.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), width),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}
