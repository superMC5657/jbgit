//! `Working Trees` modal (Spec 12 Issue 10, `ShowWorkingTreesAction` /
//! `GitWorkingTreesService`): lists `git worktree list` with branch, HEAD and
//! lock / prunable markers, and offers open (repository switch), new, lock /
//! unlock, remove and prune. Also hosts the New Worktree entry dialogs shared
//! by the branch and Log commit menus.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jbgit_core::{GitOp, Oid, RefKind, RepoSnapshot, WorktreeBranch, WorktreeInfo};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, AppState, ModalLayer, RenderContext};
use crate::dialogs::{
    ChoiceDialog, ChoiceOption, Dialog, InputDialog, InputIntent, InputValidation,
    WorktreeDialogCtx,
};
use crate::hit_test::HitTarget;
use crate::i18n::I18n;
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

/// Selection of [`ModalLayer::Worktrees`] (rows are read live from the snapshot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreesState {
    pub selected: usize,
}

impl WorktreesState {
    /// Starts on the current working tree.
    pub fn for_snapshot(snap: Option<&RepoSnapshot>) -> Self {
        let selected = snap
            .and_then(|s| s.worktrees.iter().position(|w| w.is_at(&s.workdir)))
            .unwrap_or(0);
        Self { selected }
    }
}

fn worktrees(state: &AppState) -> &[WorktreeInfo] {
    state
        .repo_snapshot
        .as_deref()
        .map_or(&[][..], |s| s.worktrees.as_slice())
}

pub fn selected_worktree<'a>(state: &'a AppState, ws: &WorktreesState) -> Option<&'a WorktreeInfo> {
    let list = worktrees(state);
    list.get(ws.selected.min(list.len().saturating_sub(1)))
}

fn top_state(state: &AppState) -> Option<WorktreesState> {
    match state.modal_stack.last() {
        Some(ModalLayer::Worktrees(ws)) => Some(*ws),
        _ => None,
    }
}

fn is_current(state: &AppState, wt: &WorktreeInfo) -> bool {
    state
        .repo_snapshot
        .as_deref()
        .is_some_and(|s| wt.is_at(&s.workdir))
}

/// Whether the detected git supports working trees (unknown = assume yes).
pub fn worktrees_supported(state: &AppState) -> bool {
    state
        .git_version
        .is_none_or(jbgit_core::GitVersion::supports_worktrees)
}

/// Notice shown instead of a worktree action on git < 2.17.
pub fn unsupported_notice(state: &AppState) -> Action {
    let found = state
        .git_version
        .map_or_else(|| "?".to_string(), |v| v.to_string());
    Action::Notice(
        state
            .i18n
            .t_fmt("toast.worktree_unsupported", &[("version", &found)]),
    )
}

/// Defaults of the New Worktree path step for the current repository.
pub fn dialog_ctx(state: &AppState) -> WorktreeDialogCtx {
    let main = state
        .repo_snapshot
        .as_deref()
        .map(|s| {
            s.main_worktree()
                .filter(|w| !w.is_bare)
                .map_or_else(|| s.workdir.clone(), |w| w.path.clone())
        })
        .unwrap_or_else(|| state.repo_path_display.clone());
    let parent = state
        .config
        .jbgit
        .worktree
        .default_parent
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .map(|p| expand_home(p.trim()));
    WorktreeDialogCtx { main, parent }
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

/// Local branches that no working tree has checked out (valid targets of
/// "existing branch").
pub fn free_local_branches(snap: &RepoSnapshot) -> Vec<String> {
    snap.refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch)
        .filter(|r| {
            !snap
                .worktrees
                .iter()
                .any(|w| w.branch.as_deref() == Some(&r.name))
        })
        .filter(|r| snap.head_branch.as_deref() != Some(r.name.as_str()))
        .map(|r| r.name.clone())
        .collect()
}

/// New Worktree chooser. `at = None` starts from `HEAD` (Worktrees modal:
/// new branch / existing branch / detached); `Some(oid)` is the Log's
/// "New Worktree from Here…" (new branch / detached at that commit).
pub fn new_worktree_dialog(state: &AppState, at: Option<Oid>) -> Dialog {
    let i18n = &state.i18n;
    let snap = state.repo_snapshot.as_deref();
    let ctx = dialog_ctx(state);
    let head = snap.and_then(|s| s.head_oid);
    let (start, start_label) = match at {
        Some(oid) => (oid.to_string(), oid.short()),
        None => ("HEAD".to_string(), "HEAD".to_string()),
    };
    let existing: Vec<String> = snap.map(free_local_branches).unwrap_or_default();
    let mut options = vec![
        ChoiceOption::new(
            i18n.t_fmt("dialog.worktree.opt_new_branch", &[("rev", &start_label)]),
            Some('n'),
            vec![Action::OpenDialog(Box::new(Dialog::Input(
                InputDialog::new(
                    i18n.t_fmt("dialog.worktree.new_branch_title", &[("rev", &start_label)]),
                    i18n.t("dialog.worktree.branch_prompt"),
                    "",
                    InputValidation::RefName {
                        what: "validation.what.branch",
                        existing: snap
                            .map(|s| {
                                s.refs
                                    .iter()
                                    .filter(|r| r.kind == RefKind::LocalBranch)
                                    .map(|r| r.name.clone())
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    InputIntent::NewWorktreeBranch {
                        start,
                        ctx: ctx.clone(),
                    },
                ),
            )))],
        )
        .detail(i18n.t("dialog.worktree.opt_new_branch_desc")),
    ];
    if at.is_none() && !existing.is_empty() {
        let first = existing.first().cloned().unwrap_or_default();
        let list = existing.join(", ");
        options.push(
            ChoiceOption::new(
                i18n.t("dialog.worktree.opt_existing"),
                Some('e'),
                vec![Action::OpenDialog(Box::new(Dialog::Input(
                    InputDialog::new(
                        i18n.t("dialog.worktree.existing_title"),
                        i18n.t_fmt("dialog.worktree.existing_prompt", &[("list", &list)]),
                        first,
                        InputValidation::OneOf {
                            candidates: existing,
                            error_key: "dialog.worktree.not_free_branch",
                        },
                        InputIntent::ExistingWorktreeBranch { ctx: ctx.clone() },
                    ),
                )))],
            )
            .detail(i18n.t("dialog.worktree.opt_existing_desc")),
        );
    }
    if let Some(oid) = at.or(head) {
        options.push(
            ChoiceOption::new(
                i18n.t_fmt("dialog.worktree.opt_detached", &[("rev", &oid.short())]),
                Some('d'),
                vec![Action::OpenDialog(Box::new(
                    ctx.path_dialog(WorktreeBranch::Detached(oid), i18n),
                ))],
            )
            .detail(i18n.t("dialog.worktree.opt_detached_desc")),
        );
    }
    Dialog::Choice(ChoiceDialog::radio(
        i18n.t("dialog.worktree.new_title"),
        vec![i18n.t("dialog.worktree.new_body").to_string()],
        options,
        0,
        i18n.t("dialog.worktree.next_btn"),
    ))
}

/// "New Worktree…" on a local branch nobody has checked out.
pub fn new_worktree_for_branch_dialog(state: &AppState, branch: &str) -> Dialog {
    dialog_ctx(state).path_dialog(WorktreeBranch::Existing(branch.to_string()), &state.i18n)
}

fn remove_confirm(wt: &WorktreeInfo, i18n: &I18n) -> Dialog {
    let path = wt.path.display().to_string();
    let mut body = vec![i18n.t_fmt("dialog.worktree.remove_body", &[("path", &path)])];
    if let Some(b) = &wt.branch {
        body.push(i18n.t_fmt("dialog.worktree.remove_branch_kept", &[("branch", b)]));
    }
    Dialog::Choice(ChoiceDialog::confirm(
        i18n.t_fmt("dialog.worktree.remove_title", &[("name", &wt.name())]),
        body,
        i18n.t("dialog.worktree.remove_btn"),
        i18n.cancel(),
        vec![Action::RunGitOp(GitOp::RemoveWorktree {
            path: wt.path.clone(),
            force: false,
        })],
    ))
}

/// Second confirmation after `git worktree remove` refused a dirty tree.
pub fn force_remove_confirm(path: &std::path::Path, i18n: &I18n) -> Dialog {
    let p = path.display().to_string();
    Dialog::Choice(ChoiceDialog::confirm(
        i18n.t("dialog.worktree.force_title"),
        vec![
            i18n.t_fmt("dialog.worktree.force_body", &[("path", &p)]),
            i18n.t("dialog.worktree.force_warning").to_string(),
        ],
        i18n.t("dialog.worktree.force_btn"),
        i18n.cancel(),
        vec![Action::RunGitOp(GitOp::RemoveWorktree {
            path: path.to_path_buf(),
            force: true,
        })],
    ))
}

fn prune_confirm(state: &AppState) -> Dialog {
    let i18n = &state.i18n;
    let prunable: Vec<String> = worktrees(state)
        .iter()
        .filter(|w| w.is_prunable())
        .map(|w| format!("  {}", w.path.display()))
        .collect();
    let mut body = vec![i18n.t("dialog.worktree.prune_body").to_string()];
    if prunable.is_empty() {
        body.push(i18n.t("dialog.worktree.prune_none_listed").to_string());
    } else {
        body.extend(prunable);
    }
    Dialog::Choice(ChoiceDialog::confirm_with_default(
        i18n.t("dialog.worktree.prune_title"),
        body,
        i18n.t("dialog.worktree.prune_btn"),
        i18n.cancel(),
        false,
        true,
        vec![Action::RunGitOp(GitOp::PruneWorktrees)],
    ))
}

fn open(d: Dialog) -> Vec<Action> {
    vec![Action::OpenDialog(Box::new(d))]
}

/// Actions for a command on the selected working tree (also used by
/// mouse double-click and the footer).
pub fn worktree_command(state: &AppState, cmd: char) -> Vec<Action> {
    let Some(ws) = top_state(state) else {
        return Vec::new();
    };
    let i18n = &state.i18n;
    match cmd {
        'n' => return open(new_worktree_dialog(state, None)),
        'p' => return open(prune_confirm(state)),
        _ => {}
    }
    let Some(wt) = selected_worktree(state, &ws) else {
        return vec![Action::Notice(
            i18n.t("view.worktrees.none_selected").into(),
        )];
    };
    let name = wt.name();
    match cmd {
        'o' => {
            if is_current(state, wt) {
                vec![Action::Notice(
                    i18n.t_fmt("toast.worktree_already_open", &[("name", &name)]),
                )]
            } else if wt.is_bare || wt.is_prunable() {
                vec![Action::Notice(
                    i18n.t_fmt("toast.worktree_cannot_open", &[("name", &name)]),
                )]
            } else {
                vec![Action::PopModal, Action::SwitchRepository(wt.path.clone())]
            }
        }
        'l' => {
            if wt.is_main {
                vec![Action::Notice(i18n.t("toast.worktree_main_lock").into())]
            } else if wt.is_locked() {
                vec![Action::RunGitOp(GitOp::UnlockWorktree {
                    path: wt.path.clone(),
                })]
            } else {
                open(Dialog::Input(InputDialog::new(
                    i18n.t_fmt("dialog.worktree.lock_title", &[("name", &name)]),
                    i18n.t("dialog.worktree.lock_prompt"),
                    "",
                    InputValidation::None,
                    InputIntent::LockWorktree {
                        path: wt.path.clone(),
                    },
                )))
            }
        }
        'd' => {
            if wt.is_main || is_current(state, wt) {
                vec![Action::Notice(
                    i18n.t_fmt("toast.worktree_cannot_remove", &[("name", &name)]),
                )]
            } else if wt.is_locked() {
                vec![Action::Notice(i18n.t_fmt(
                    "toast.worktree_locked_unlock_first",
                    &[("name", &name)],
                ))]
            } else {
                open(remove_confirm(wt, i18n))
            }
        }
        'y' => vec![Action::CopyText(wt.path.display().to_string())],
        _ => Vec::new(),
    }
}

/// Keyboard routing while the Worktrees modal is the topmost layer.
pub fn route_worktrees_key(key: &KeyEvent, state: &AppState) -> Vec<Action> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => vec![Action::PopModal],
        KeyCode::Up | KeyCode::Char('k') => vec![Action::WorktreesMove(-1)],
        KeyCode::Down | KeyCode::Char('j') => vec![Action::WorktreesMove(1)],
        KeyCode::Home | KeyCode::Char('g') => vec![Action::WorktreesSelect(0)],
        KeyCode::End | KeyCode::Char('G') => vec![Action::WorktreesSelect(usize::MAX)],
        KeyCode::Enter | KeyCode::Char('o') => worktree_command(state, 'o'),
        KeyCode::Char('n' | 'a') | KeyCode::Insert => worktree_command(state, 'n'),
        KeyCode::Char('l') => worktree_command(state, 'l'),
        KeyCode::Char('d') | KeyCode::Delete => worktree_command(state, 'd'),
        KeyCode::Char('p') => worktree_command(state, 'p'),
        KeyCode::Char('y') => worktree_command(state, 'y'),
        _ => Vec::new(),
    }
}

/// Applies a selection change to the topmost Worktrees layer.
pub fn apply_selection(state: &mut AppState, select: Option<usize>, delta: isize) {
    let len = worktrees(state).len();
    if let Some(ModalLayer::Worktrees(ws)) = state.modal_stack.last_mut() {
        let cur = ws.selected.min(len.saturating_sub(1));
        ws.selected = match select {
            Some(idx) => idx.min(len.saturating_sub(1)),
            None => (cur as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize,
        };
    }
}

pub fn render_worktrees_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ws: &WorktreesState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 24 || screen.height < 8 {
        return;
    }
    let list = worktrees(state);
    let w = screen
        .width
        .saturating_sub(4)
        .clamp(24.min(screen.width), 110);
    let h = (list.len().max(1) as u16 * 2 + 5)
        .clamp(9, 26)
        .min(screen.height.saturating_sub(2).max(6));
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    );
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);
    let title = format!(" {} ", ctx.t("view.worktrees.title"));
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
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let width = inner.width as usize;
    let list_h = inner.height.saturating_sub(1);

    if list.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.worktrees.empty")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    } else {
        let selected = ws.selected.min(list.len() - 1);
        let per_page = (list_h / 2).max(1) as usize;
        let first = selected.saturating_sub(per_page.saturating_sub(1));
        let name_w = list
            .iter()
            .map(|wt| wt.name().width())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        let detached = ctx.t("view.worktrees.detached").to_string();
        for (row, (idx, wt)) in list
            .iter()
            .enumerate()
            .skip(first)
            .take(per_page)
            .enumerate()
        {
            let y = inner.y + (row as u16) * 2;
            let bg = if idx == selected {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let current = is_current(state, wt);
            let marker = if current { "● " } else { "  " };
            let name = truncate_to_width(&wt.name(), name_w);
            let pad = " ".repeat(name_w.saturating_sub(name.width()));
            let branch = match (&wt.branch, wt.is_bare) {
                (_, true) => ctx.t("view.worktrees.bare").to_string(),
                (Some(b), _) => b.clone(),
                (None, _) => detached.clone(),
            };
            let head = wt.head.map(|h| h.short()).unwrap_or_default();
            let mut line1 = vec![
                Span::styled(
                    format!(" {marker}"),
                    Style::default().fg(ctx.theme.fg_accent_link),
                ),
                Span::styled(
                    format!("{name}{pad}  "),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    branch,
                    Style::default().fg(if wt.branch.is_some() {
                        ctx.theme.ref_head.pill_fg
                    } else {
                        ctx.theme.fg_muted
                    }),
                ),
                Span::styled(format!("  {head}"), Style::default().fg(ctx.theme.fg_muted)),
            ];
            let mut tags = Vec::new();
            if wt.is_main {
                tags.push(ctx.t("view.worktrees.tag_main").to_string());
            }
            if let Some(reason) = &wt.locked {
                tags.push(if reason.is_empty() {
                    ctx.t("view.worktrees.tag_locked").to_string()
                } else {
                    ctx.t_fmt("view.worktrees.tag_locked_reason", &[("reason", reason)])
                });
            }
            if wt.is_prunable() {
                tags.push(ctx.t("view.worktrees.tag_prunable").to_string());
            }
            if !tags.is_empty() {
                line1.push(Span::styled(
                    format!("  [{}]", tags.join("] [")),
                    Style::default().fg(if wt.is_prunable() {
                        ctx.theme.status_deleted
                    } else {
                        ctx.theme.status_modified
                    }),
                ));
            }
            let line2 = Line::from(Span::styled(
                truncate_to_width(&format!("    {}", wt.path.display()), width),
                Style::default().fg(ctx.theme.fg_muted),
            ));
            let row_rect = Rect::new(inner.x, y, inner.width, 2.min(list_h - row as u16 * 2));
            frame.render_widget(
                Paragraph::new(vec![Line::from(line1), line2]).style(Style::default().bg(bg)),
                row_rect,
            );
            ctx.hit_registry
                .register(row_rect, z + 2, HitTarget::WorktreesRow(idx));
        }
    }

    let hint = ctx.t("view.worktrees.hint").to_string();
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&format!(" {hint}"), width),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}
