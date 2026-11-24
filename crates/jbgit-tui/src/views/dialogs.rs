//! Rendering of [`Dialog`] layers: validated input, radio / button choice,
//! multi-line message editor and the interactive-rebase todo table.

use jbgit_core::{DiffContext, DiffLineKind, FileDiffKey, FileStatusKind, TodoAction};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::dialogs::{
    ChoiceDialog, CreatePatchDialog, CreatePatchRow, CredentialDialog, Dialog, IdentityDialog,
    IdentityField, InputDialog, MergeDialog, MergeDialogRow, MessageDialog, NewBranchDialog,
    NewBranchField, PullDialog, PullDialogRow, RebaseDialog, RebaseDialogRow, RebaseTodoDialog,
    StashPushDialog, StashPushField,
};
use crate::hit_test::{DialogButtonKind, HitTarget};
use crate::views::overlays::{draw_shadow, mnemonic_spans};
use crate::views::{truncate_to_width, wrap_text};

fn centered(screen: Rect, w: u16, h: u16) -> Rect {
    let w = w
        .min(screen.width.saturating_sub(2))
        .max(10.min(screen.width));
    let h = h
        .min(screen.height.saturating_sub(2))
        .max(5.min(screen.height));
    Rect::new(
        screen.x + screen.width.saturating_sub(w) / 2,
        screen.y + screen.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

fn frame_block(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    rect: Rect,
    title: &str,
    danger: bool,
    z: u8,
) -> Rect {
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);
    let accent = if danger {
        ctx.theme.status_conflicted
    } else {
        ctx.theme.border_focused
    };
    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if danger { accent } else { ctx.theme.fg_bright })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);
    inner
}

struct DialogButtonSpec {
    label: String,
    selected: bool,
    enabled: bool,
    danger: bool,
    target: HitTarget,
}

/// Renders `[label]` buttons left-to-right starting at `(x, y)`.
fn render_buttons(
    frame: &mut Frame<'_>,
    ctx: &mut RenderContext<'_>,
    x: u16,
    y: u16,
    max_right: u16,
    buttons: &[DialogButtonSpec],
    z: u8,
) {
    let mut cx = x;
    for b in buttons {
        let text = if b.selected && b.enabled {
            format!("[{}]", b.label)
        } else {
            format!(" {} ", b.label)
        };
        let w = text.width() as u16;
        if cx + w > max_right {
            break;
        }
        let style = if !b.enabled {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        } else if b.selected {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(if b.danger {
                    ctx.theme.status_conflicted
                } else {
                    ctx.theme.border_focused
                })
                .add_modifier(Modifier::BOLD)
        } else if b.danger {
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .bg(ctx.theme.bg_panel)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        let r = Rect::new(cx, y, w, 1);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), r);
        ctx.hit_registry.register(r, z + 2, b.target.clone());
        cx += w + 2;
    }
}

pub fn render_dialog(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    dialog: &Dialog,
    z: u8,
) {
    if screen.width < 20 || screen.height < 8 {
        return;
    }
    match dialog {
        Dialog::Input(d) => render_input(frame, screen, ctx, d, z),
        Dialog::NewBranch(d) => render_new_branch(frame, screen, ctx, d, z),
        Dialog::StashPush(d) => render_stash_push(frame, screen, ctx, d, z),
        Dialog::Choice(d) => render_choice(frame, screen, ctx, d, z),
        Dialog::Message(d) => render_message(frame, screen, ctx, d, z),
        Dialog::RebaseTodo(d) => render_rebase(frame, screen, state, ctx, d, z),
        Dialog::Merge(d) => render_merge(frame, screen, ctx, d, z),
        Dialog::Pull(d) => render_pull(frame, screen, ctx, d, z),
        Dialog::Rebase(d) => render_rebase_options(frame, screen, ctx, d, z),
        Dialog::CreatePatch(d) => render_create_patch(frame, screen, ctx, d, z),
        Dialog::Credential(d) => render_credential(frame, screen, ctx, d, z),
        Dialog::Identity(d) => render_identity(frame, screen, ctx, d, z),
    }
}

fn render_new_branch(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &NewBranchDialog,
    z: u8,
) {
    let rect = centered(screen, 70, 14);
    let inner = frame_block(frame, screen, ctx, rect, &d.title, false, z);
    let w = inner.width as usize;
    let err = ctx.i18n.and_then(|i| d.error(i));
    let mut y = inner.y;

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&d.prompt, w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    let name_focused = d.focused == NewBranchField::Name;
    let field_bg = if name_focused {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_panel
    };
    let shown = if name_focused {
        format!(" {}▌", d.value)
    } else {
        format!(" {}", d.value)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&shown, w),
            Style::default().fg(ctx.theme.fg_bright).bg(field_bg),
        ))
        .style(Style::default().bg(field_bg)),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    if let Some(msg) = &err {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(msg, w),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
    y += 1;

    let rows = [
        (
            NewBranchField::Checkout,
            d.checkout,
            ctx.t("dialog.new_branch.checkout_checkbox").to_string(),
        ),
        (
            NewBranchField::Overwrite,
            d.overwrite,
            ctx.t("dialog.new_branch.overwrite_checkbox").to_string(),
        ),
        (
            NewBranchField::Track,
            d.track,
            ctx.t("dialog.new_branch.track_checkbox").to_string(),
        ),
        (
            NewBranchField::AutoSanitize,
            d.auto_sanitize,
            ctx.t("dialog.new_branch.sanitize_checkbox").to_string(),
        ),
    ];
    for (idx, (field, on, label)) in rows.into_iter().enumerate() {
        if y + 2 > inner.bottom() {
            break;
        }
        let focused = d.focused == field;
        let bg = if focused {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if on { " [✓] " } else { " [ ] " },
                    Style::default().fg(ctx.theme.fg_accent_link).bg(bg),
                ),
                Span::styled(
                    truncate_to_width(&label, w.saturating_sub(6)),
                    Style::default().fg(ctx.theme.fg_primary).bg(bg),
                ),
            ]))
            .style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(idx));
        y += 1;
    }

    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.ok().into(),
                selected: true,
                enabled: err.is_none(),
                danger: d.overwrite,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_stash_push(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &StashPushDialog,
    z: u8,
) {
    let h = if d.selected_paths.is_empty() { 13 } else { 14 };
    let rect = centered(screen, 72, h);
    let title = ctx.t("dialog.stash.new_title").to_string();
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;
    let mut y = inner.y;

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(ctx.t("dialog.stash.new_prompt"), w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    let msg_focused = d.focused == StashPushField::Message;
    let field_bg = if msg_focused {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_panel
    };
    let shown = if msg_focused {
        format!(" {}▌", d.message)
    } else {
        format!(" {}", d.message)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&shown, w),
            Style::default().fg(ctx.theme.fg_bright).bg(field_bg),
        ))
        .style(Style::default().bg(field_bg)),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 2;

    let mut checkboxes = vec![
        (
            StashPushField::IncludeUntracked,
            d.include_untracked,
            ctx.t("dialog.stash.include_untracked").to_string(),
        ),
        (
            StashPushField::KeepIndex,
            d.keep_index,
            ctx.t("dialog.stash.keep_index").to_string(),
        ),
    ];
    if !d.selected_paths.is_empty() {
        let count_s = d.selected_paths.len().to_string();
        let lbl = ctx.i18n.map_or_else(
            || format!("Only selected files ({count_s})"),
            |i| i.t_fmt("dialog.stash.only_selected", &[("count", &count_s)]),
        );
        checkboxes.push((StashPushField::OnlySelected, d.only_selected, lbl));
    }
    for (idx, (field, on, label)) in checkboxes.into_iter().enumerate() {
        if y + 3 > inner.bottom() {
            break;
        }
        let focused = d.focused == field;
        let bg = if focused {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if on { " [✓] " } else { " [ ] " },
                    Style::default().fg(ctx.theme.fg_accent_link).bg(bg),
                ),
                Span::styled(
                    truncate_to_width(&label, w.saturating_sub(6)),
                    Style::default().fg(ctx.theme.fg_primary).bg(bg),
                ),
            ]))
            .style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(idx));
        y += 1;
    }

    if y + 1 < inner.bottom() {
        let preview = d.preview_command();
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&preview, w),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
        );
    }

    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.t("dialog.stash.stash_btn").into(),
                selected: true,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_identity(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &IdentityDialog,
    z: u8,
) {
    let rect = centered(screen, 68, 12);
    let title = ctx.t("dialog.identity.title").to_string();
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;
    let err = ctx.i18n.and_then(|i| d.error(i));

    let name_cur = if d.focused == IdentityField::Name { "▏" } else { "" };
    let email_cur = if d.focused == IdentityField::Email { "▏" } else { "" };
    let field_style = |active: bool| {
        if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        }
    };

    let lines = vec![
        Line::from(Span::styled(
            format!(" {}", truncate_to_width(ctx.t("dialog.identity.subtitle"), w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Line::from(Span::styled(
            format!(" {}", ctx.t("dialog.identity.name_label")),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Line::from(Span::styled(
            format!(
                " {}{name_cur}{}",
                truncate_to_width(&d.name, w.saturating_sub(4)),
                " ".repeat(w.saturating_sub(d.name.width() + 3))
            ),
            field_style(d.focused == IdentityField::Name),
        )),
        Line::from(Span::styled(
            format!(" {}", ctx.t("dialog.identity.email_label")),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Line::from(Span::styled(
            format!(
                " {}{email_cur}{}",
                truncate_to_width(&d.email, w.saturating_sub(4)),
                " ".repeat(w.saturating_sub(d.email.width() + 3))
            ),
            field_style(d.focused == IdentityField::Email),
        )),
        Line::from(Span::styled(
            format!(
                " [{}] {}   (Space)",
                if d.global { "x" } else { " " },
                truncate_to_width(ctx.t("dialog.identity.global_checkbox"), w.saturating_sub(15))
            ),
            Style::default()
                .fg(if d.focused == IdentityField::Global {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                })
                .bg(if d.focused == IdentityField::Global {
                    ctx.theme.bg_selection_focused
                } else {
                    ctx.theme.bg_elevated
                }),
        )),
        match &err {
            Some(e) => Line::from(Span::styled(
                format!(" ✕ {}", truncate_to_width(e, w.saturating_sub(4))),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            None => Line::from(Span::styled(
                format!(" ✓ {}", ctx.t("dialog.identity.hint")),
                Style::default().fg(ctx.theme.status_added),
            )),
        },
    ];
    frame.render_widget(Paragraph::new(lines), inner);
    ctx.hit_registry
        .register(Rect::new(inner.x, inner.y + 2, inner.width, 1), z + 2, HitTarget::DialogOption(0));
    ctx.hit_registry
        .register(Rect::new(inner.x, inner.y + 4, inner.width, 1), z + 2, HitTarget::DialogOption(1));
    ctx.hit_registry
        .register(Rect::new(inner.x, inner.y + 5, inner.width, 1), z + 2, HitTarget::DialogCheckbox);

    let confirm_label = if d.pending_commit.is_some() {
        ctx.t("dialog.identity.save_and_commit_btn").into()
    } else {
        ctx.ok().into()
    };
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: confirm_label,
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_credential(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &CredentialDialog,
    z: u8,
) {
    let shown = d.display_value();
    if d.host_key_confirm {
        // SSH host-key verification: display the full multi-line fingerprint
        // text verbatim (unmasked) and guide the user to confirm `yes`/`no`.
        let max_w = 78usize.min(screen.width.saturating_sub(4).max(32) as usize);
        let wrap_w = max_w.saturating_sub(4).max(20);
        let mut prompt_lines: Vec<String> = Vec::new();
        for raw in d.prompt.lines() {
            let trimmed = raw.trim_end();
            if trimmed.is_empty() {
                prompt_lines.push(String::new());
                continue;
            }
            let mut cur = String::new();
            for word in trimmed.split_inclusive(' ') {
                if !cur.is_empty() && cur.width() + word.width() > wrap_w {
                    prompt_lines.push(cur.trim_end().to_string());
                    cur.clear();
                }
                cur.push_str(word);
            }
            if !cur.is_empty() {
                prompt_lines.push(cur.trim_end().to_string());
            }
        }
        if prompt_lines.is_empty() {
            prompt_lines.push(d.prompt.clone());
        }
        let h = (prompt_lines.len() as u16 + 7)
            .clamp(10, 18)
            .min(screen.height.saturating_sub(2).max(8));
        let rect = centered(screen, max_w as u16, h);
        let title = ctx.t("dialog.credential.host_key_title").to_string();
        let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
        let w = inner.width as usize;
        let max_prompt_rows = inner.height.saturating_sub(5) as usize;
        let mut lines: Vec<Line<'_>> = prompt_lines
            .into_iter()
            .take(max_prompt_rows)
            .map(|l| {
                Line::from(Span::styled(
                    format!(" {}", truncate_to_width(&l, w.saturating_sub(2))),
                    Style::default()
                        .fg(ctx.theme.fg_primary)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect();
        lines.push(Line::from(Span::styled(
            format!(" {}▏", truncate_to_width(&shown, w.saturating_sub(4)))
                + &" ".repeat(w.saturating_sub(shown.width() + 3)),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::UNDERLINED),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                " {}",
                truncate_to_width(ctx.t("dialog.credential.hint_host_key"), w.saturating_sub(2))
            ),
            Style::default().fg(ctx.theme.fg_muted),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
        let by = inner.bottom().saturating_sub(1);
        render_buttons(
            frame,
            ctx,
            inner.x + 1,
            by,
            inner.right(),
            &[
                DialogButtonSpec {
                    label: ctx.ok().into(),
                    selected: true,
                    enabled: true,
                    danger: false,
                    target: HitTarget::DialogButton(DialogButtonKind::Confirm),
                },
                DialogButtonSpec {
                    label: ctx.cancel().into(),
                    selected: false,
                    enabled: true,
                    danger: false,
                    target: HitTarget::DialogButton(DialogButtonKind::Cancel),
                },
            ],
            z,
        );
        return;
    }

    let rect = centered(screen, 72, 10);
    let title = ctx.t("dialog.credential.title").to_string();
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;
    let hint_key = if !d.masked {
        "dialog.credential.hint_plain"
    } else if d.revealed {
        "dialog.credential.hint_revealed"
    } else {
        "dialog.credential.hint_masked"
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(" {}", truncate_to_width(&d.prompt, w.saturating_sub(2))),
            Style::default()
                .fg(ctx.theme.fg_primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(" {}▏", truncate_to_width(&shown, w.saturating_sub(4)))
                + &" ".repeat(w.saturating_sub(shown.width() + 3)),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::UNDERLINED),
        )),
        Line::from(Span::styled(
            format!(" {}", truncate_to_width(ctx.t(hint_key), w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Line::from(Span::styled(
            format!(" {}", truncate_to_width(ctx.t("dialog.credential.no_store"), w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_muted),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
    let by = inner.bottom().saturating_sub(1);
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        by,
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.ok().into(),
                selected: true,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_merge(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &MergeDialog,
    z: u8,
) {
    let rows = MergeDialogRow::ALL.len() as u16;
    let rect = centered(screen, 78, rows + 9);
    let title = ctx.t_fmt(
        "dialog.merge.title",
        &[("rev", &d.rev), ("cur", &d.current)],
    );
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;
    let count_line = match d.commit_count {
        Some(n) => {
            let n_s = n.to_string();
            ctx.t_fmt("dialog.merge.commit_count", &[("count", &n_s), ("rev", &d.rev), ("cur", &d.current)])
        }
        None => ctx.t("dialog.merge.commit_count_unknown").to_string(),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", truncate_to_width(&count_line, w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let o = &d.options;
    let none_lbl = ctx.t("dialog.merge.none").to_string();
    let label_w = 30usize;
    for (i, row) in MergeDialogRow::ALL.iter().copied().enumerate() {
        let y = inner.y + 2 + i as u16;
        if y >= inner.bottom().saturating_sub(2) {
            break;
        }
        let sel = i == d.selected;
        let (label, value, detail): (String, String, String) = match row {
            MergeDialogRow::FastForward => (
                ctx.t("dialog.merge.row_ff").to_string(),
                format!(
                    "‹ {} ›",
                    match o.ff.flag() {
                        Some(f) => f.to_string(),
                        None => ctx.t("dialog.merge.ff_default").to_string(),
                    }
                ),
                String::new(),
            ),
            MergeDialogRow::Squash => (
                format!("[{}] --squash", if o.squash { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.merge.squash_desc").to_string(),
            ),
            MergeDialogRow::NoCommit => (
                format!("[{}] --no-commit", if o.no_commit { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.merge.no_commit_desc").to_string(),
            ),
            MergeDialogRow::AllowUnrelated => (
                format!(
                    "[{}] --allow-unrelated-histories",
                    if o.allow_unrelated { "x" } else { " " }
                ),
                String::new(),
                String::new(),
            ),
            MergeDialogRow::Strategy => (
                ctx.t("dialog.merge.row_strategy").to_string(),
                format!("‹ {} ›", o.strategy.as_deref().unwrap_or(&none_lbl)),
                String::new(),
            ),
            MergeDialogRow::StrategyOption => (
                ctx.t("dialog.merge.row_strategy_option").to_string(),
                format!("‹ {} ›", o.strategy_option.as_deref().unwrap_or(&none_lbl)),
                String::new(),
            ),
            MergeDialogRow::Message => {
                let text = d.message_text().replace('\n', "⏎");
                let cursor = if sel { "▏" } else { "" };
                let shown = if text.is_empty() && !sel {
                    ctx.t("dialog.merge.message_placeholder").to_string()
                } else {
                    let avail = w.saturating_sub(label_w + 4);
                    let tail: String = text.chars().rev().collect();
                    let tail = truncate_to_width(&tail, avail);
                    tail.chars().rev().collect::<String>()
                };
                (
                    ctx.t("dialog.merge.row_message").to_string(),
                    format!("{shown}{cursor}"),
                    String::new(),
                )
            }
        };
        let bg = if sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let mnemonic = row.mnemonic();
        let mut spans = vec![Span::styled(
            format!(" {mnemonic} "),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        )];
        let pad = label_w.saturating_sub(label.width());
        spans.push(Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(ctx.theme.fg_bright),
        ));
        if !value.is_empty() {
            let value_style = if row == MergeDialogRow::Message {
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_panel)
            } else {
                Style::default().fg(ctx.theme.fg_accent_link)
            };
            spans.push(Span::styled(value, value_style));
        }
        if !detail.is_empty() {
            spans.push(Span::styled(
                format!("  {}", truncate_to_width(&detail, w.saturating_sub(label_w + 6))),
                Style::default().fg(ctx.theme.fg_muted),
            ));
        }
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(i));
    }
    let err = ctx.i18n.and_then(|i| d.error(i));
    let (hint, color) = match &err {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
        None => {
            let summary = d.options.summary();
            let cmd = if summary.is_empty() {
                format!(" git merge {}", d.rev)
            } else {
                format!(" git merge {summary} {}", d.rev)
            };
            (cmd, ctx.theme.fg_muted)
        }
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hint, w),
            Style::default().fg(color),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(ctx.t("dialog.merge.hint"), w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
    );
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.t("dialog.merge.merge_btn").into(),
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_pull(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &PullDialog,
    z: u8,
) {
    let rows = PullDialogRow::ALL.len() as u16;
    let rect = centered(screen, 80, rows + 9);
    let title = ctx.t_fmt("dialog.pull.title", &[("cur", &d.current)]);
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;

    let remote_branches = d.branches_for_selected_remote();
    let status_line = if d.fetching {
        ctx.t("dialog.pull.fetching").to_string()
    } else {
        let count_s = remote_branches.len().to_string();
        let rem = d
            .options
            .effective_remote()
            .unwrap_or_else(|| ctx.t("dialog.pull.remote_default"));
        ctx.t_fmt(
            "dialog.pull.subtitle",
            &[("cur", &d.current), ("remote", rem), ("count", &count_s)],
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", truncate_to_width(&status_line, w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let o = &d.options;
    let label_w = 24usize;
    for (i, row) in PullDialogRow::ALL.iter().copied().enumerate() {
        let y = inner.y + 2 + i as u16;
        if y >= inner.bottom().saturating_sub(3) {
            break;
        }
        let sel = i == d.selected;
        let (label, value, detail): (String, String, String) = match row {
            PullDialogRow::Remote => (
                ctx.t("dialog.pull.row_remote").to_string(),
                format!(
                    "‹ {} ›",
                    o.effective_remote()
                        .unwrap_or_else(|| ctx.t("dialog.pull.remote_default"))
                ),
                String::new(),
            ),
            PullDialogRow::Branch => {
                let text = d.branch_text();
                let cursor = if sel { "▏" } else { "" };
                let shown = if text.is_empty() && !sel {
                    ctx.t("dialog.pull.branch_placeholder").to_string()
                } else {
                    truncate_to_width(text, w.saturating_sub(label_w + 8))
                };
                let hint = if remote_branches.is_empty() {
                    String::new()
                } else {
                    format!("‹{}›", remote_branches.len())
                };
                (
                    ctx.t("dialog.pull.row_branch").to_string(),
                    format!("{shown}{cursor}"),
                    hint,
                )
            }
            PullDialogRow::Rebase => (
                format!("[{}] --rebase", if o.rebase { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.pull.rebase_desc").to_string(),
            ),
            PullDialogRow::FastForward => (
                ctx.t("dialog.merge.row_ff").to_string(),
                format!(
                    "‹ {} ›",
                    match o.ff.flag() {
                        Some(f) => f.to_string(),
                        None => ctx.t("dialog.merge.ff_default").to_string(),
                    }
                ),
                String::new(),
            ),
            PullDialogRow::Squash => (
                format!("[{}] --squash", if o.squash { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.merge.squash_desc").to_string(),
            ),
            PullDialogRow::NoCommit => (
                format!("[{}] --no-commit", if o.no_commit { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.merge.no_commit_desc").to_string(),
            ),
            PullDialogRow::NoVerify => (
                format!("[{}] --no-verify", if o.no_verify { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.pull.no_verify_desc").to_string(),
            ),
        };
        let bg = if sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let mnemonic = row.mnemonic();
        let mut spans = vec![Span::styled(
            format!(" {mnemonic} "),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        )];
        let pad = label_w.saturating_sub(label.width());
        spans.push(Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(ctx.theme.fg_bright),
        ));
        if !value.is_empty() {
            let value_style = if row == PullDialogRow::Branch {
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_panel)
            } else {
                Style::default().fg(ctx.theme.fg_accent_link)
            };
            spans.push(Span::styled(value, value_style));
        }
        if !detail.is_empty() {
            spans.push(Span::styled(
                format!(
                    "  {}",
                    truncate_to_width(&detail, w.saturating_sub(label_w + 6))
                ),
                Style::default().fg(ctx.theme.fg_muted),
            ));
        }
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(i));
    }

    let err = ctx.i18n.and_then(|i| d.error(i));
    let (hint, color) = match &err {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
        None => {
            let summary = d.options.summary();
            let cmd = if summary.is_empty() {
                " git pull --no-edit".to_string()
            } else {
                format!(" git pull {summary}")
            };
            (cmd, ctx.theme.fg_muted)
        }
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hint, w),
            Style::default().fg(color),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(ctx.t("dialog.pull.hint"), w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
    );
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.t("dialog.pull.pull_btn").into(),
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.t("dialog.pull.fetch_btn").into(),
                selected: false,
                enabled: !d.fetching,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Fetch),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_rebase_options(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &RebaseDialog,
    z: u8,
) {
    let rows = RebaseDialogRow::ALL.len() as u16;
    let rect = centered(screen, 82, rows + 9);
    let title = ctx.t_fmt("dialog.rebase_opts.title", &[("cur", &d.current)]);
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let w = inner.width as usize;

    let subtitle = ctx.t_fmt("dialog.rebase_opts.subtitle", &[("cur", &d.current)]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {}", truncate_to_width(&subtitle, w.saturating_sub(2))),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let o = &d.options;
    let label_w = 24usize;
    let has_upstream = o.effective_upstream().is_some();
    for (i, row) in RebaseDialogRow::ALL.iter().copied().enumerate() {
        let y = inner.y + 2 + i as u16;
        if y >= inner.bottom().saturating_sub(3) {
            break;
        }
        let sel = i == d.selected;
        let disabled = row == RebaseDialogRow::Upstream && o.root;
        let (label, value, detail): (String, String, String) = match row {
            RebaseDialogRow::Onto => {
                let lbl = if has_upstream || o.root {
                    ctx.t("dialog.rebase_opts.row_onto_new")
                } else {
                    ctx.t("dialog.rebase_opts.row_onto")
                };
                let text = o.onto.as_str();
                let cursor = if sel { "▏" } else { "" };
                let shown = if text.is_empty() && !sel {
                    ctx.t("dialog.rebase_opts.onto_placeholder").to_string()
                } else {
                    truncate_to_width(text, w.saturating_sub(label_w + 8))
                };
                (lbl.to_string(), format!("{shown}{cursor}"), String::new())
            }
            RebaseDialogRow::Upstream => {
                let text = o.upstream.as_deref().unwrap_or("");
                let cursor = if sel && !disabled { "▏" } else { "" };
                let shown = if disabled {
                    ctx.t("dialog.rebase_opts.disabled_by_root").to_string()
                } else if text.is_empty() && !sel {
                    ctx.t("dialog.rebase_opts.upstream_placeholder").to_string()
                } else {
                    truncate_to_width(text, w.saturating_sub(label_w + 8))
                };
                (
                    ctx.t("dialog.rebase_opts.row_upstream").to_string(),
                    format!("{shown}{cursor}"),
                    String::new(),
                )
            }
            RebaseDialogRow::Branch => {
                let text = o.branch.as_deref().unwrap_or("");
                let cursor = if sel { "▏" } else { "" };
                let shown = if text.is_empty() && !sel {
                    ctx.t_fmt("dialog.rebase_opts.branch_placeholder", &[("cur", &d.current)])
                } else {
                    truncate_to_width(text, w.saturating_sub(label_w + 8))
                };
                (
                    ctx.t("dialog.rebase_opts.row_branch").to_string(),
                    format!("{shown}{cursor}"),
                    String::new(),
                )
            }
            RebaseDialogRow::Interactive => (
                format!("[{}] --interactive", if o.interactive { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.interactive_desc").to_string(),
            ),
            RebaseDialogRow::RebaseMerges => (
                format!("[{}] --rebase-merges", if o.rebase_merges { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.rebase_merges_desc").to_string(),
            ),
            RebaseDialogRow::UpdateRefs => (
                format!("[{}] --update-refs", if o.update_refs { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.update_refs_desc").to_string(),
            ),
            RebaseDialogRow::KeepEmpty => (
                format!("[{}] --keep-empty", if o.keep_empty { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.keep_empty_desc").to_string(),
            ),
            RebaseDialogRow::Autosquash => (
                format!("[{}] --autosquash", if o.autosquash { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.autosquash_desc").to_string(),
            ),
            RebaseDialogRow::Root => (
                format!("[{}] --root", if o.root { "x" } else { " " }),
                String::new(),
                ctx.t("dialog.rebase_opts.root_desc").to_string(),
            ),
        };
        let bg = if sel {
            ctx.theme.bg_selection_focused
        } else {
            ctx.theme.bg_elevated
        };
        let mnemonic = row.mnemonic();
        let mut spans = vec![Span::styled(
            format!(" {mnemonic} "),
            Style::default()
                .fg(if disabled {
                    ctx.theme.fg_muted
                } else {
                    ctx.theme.fg_accent_link
                })
                .add_modifier(Modifier::UNDERLINED),
        )];
        let pad = label_w.saturating_sub(label.width());
        spans.push(Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            Style::default().fg(if disabled {
                ctx.theme.fg_muted
            } else {
                ctx.theme.fg_bright
            }),
        ));
        if !value.is_empty() {
            let value_style = if disabled {
                Style::default().fg(ctx.theme.fg_muted)
            } else if row.is_text() {
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_panel)
            } else {
                Style::default().fg(ctx.theme.fg_accent_link)
            };
            spans.push(Span::styled(value, value_style));
        }
        if !detail.is_empty() {
            spans.push(Span::styled(
                format!(
                    "  {}",
                    truncate_to_width(&detail, w.saturating_sub(label_w + 6))
                ),
                Style::default().fg(ctx.theme.fg_muted),
            ));
        }
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(i));
    }

    let err = ctx.i18n.and_then(|i| d.error(i));
    let (hint, color) = match &err {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
        None => {
            let summary = d.options.summary();
            let cmd = if summary.is_empty() {
                " git rebase".to_string()
            } else {
                format!(" git rebase {summary}")
            };
            (cmd, ctx.theme.fg_muted)
        }
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hint, w),
            Style::default().fg(color),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(ctx.t("dialog.rebase_opts.hint"), w),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
    );
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.t("dialog.rebase_opts.start_btn").into(),
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_input(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &InputDialog,
    z: u8,
) {
    let h = 8 + u16::from(d.checkbox.is_some()) + u16::from(d.secondary_checkbox.is_some());
    let rect = centered(screen, 64, h);
    let inner = frame_block(frame, screen, ctx, rect, &d.title, false, z);
    let w = inner.width as usize;
    let err = ctx.i18n.and_then(|i| d.error(i));
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {}", d.prompt),
            Style::default().fg(ctx.theme.fg_primary),
        )),
        Line::from(Span::styled(
            format!(" {}▏", truncate_to_width(&d.value, w.saturating_sub(4)))
                + &" ".repeat(w.saturating_sub(d.value.width() + 3)),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::UNDERLINED),
        )),
        match &err {
            Some(e) => Line::from(Span::styled(
                format!(" ✕ {}", truncate_to_width(e, w.saturating_sub(4))),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            None => Line::from(Span::styled(
                " ✓",
                Style::default().fg(ctx.theme.status_added),
            )),
        },
    ];
    let mut next_cb_y = inner.y + 3;
    if let Some((label, on)) = &d.checkbox {
        lines.push(Line::from(Span::styled(
            format!(
                " [{}] {label}   ({})",
                if *on { "x" } else { " " },
                ctx.t("dialog.checkbox.tab_hint")
            ),
            Style::default().fg(ctx.theme.fg_primary),
        )));
        ctx.hit_registry.register(
            Rect::new(inner.x, next_cb_y, inner.width, 1),
            z + 2,
            HitTarget::DialogCheckbox,
        );
        next_cb_y += 1;
    }
    if let Some((label, on)) = &d.secondary_checkbox {
        lines.push(Line::from(Span::styled(
            format!(
                " [{}] {label}   ({})",
                if *on { "x" } else { " " },
                ctx.t("dialog.checkbox.ctrl_s_hint")
            ),
            Style::default().fg(ctx.theme.fg_primary),
        )));
        ctx.hit_registry.register(
            Rect::new(inner.x, next_cb_y, inner.width, 1),
            z + 2,
            HitTarget::DialogSecondaryCheckbox,
        );
    }
    frame.render_widget(Paragraph::new(lines), inner);
    let by = inner.bottom().saturating_sub(1);
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        by,
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.ok().into(),
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_choice(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &ChoiceDialog,
    z: u8,
) {
    let width: u16 = 72;
    let inner_w = width.saturating_sub(4) as usize;
    let body: Vec<String> = d.body.iter().flat_map(|l| wrap_text(l, inner_w)).collect();
    let opt_rows = if d.radio {
        d.options.len() * 2
    } else {
        d.options.len()
    };
    let cb_rows = if d.checkbox.is_some() { 2 } else { 0 };
    let h = (body.len() + opt_rows + cb_rows + 5) as u16;
    let rect = centered(screen, width, h);
    let inner = frame_block(frame, screen, ctx, rect, &d.title, d.danger, z);
    let mut y = inner.y;
    for l in &body {
        if y >= inner.bottom() {
            break;
        }
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", truncate_to_width(l, inner_w)),
                Style::default().fg(ctx.theme.fg_primary),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }
    y += 1;
    if let Some(cb) = &d.checkbox
        && y < inner.bottom().saturating_sub(2)
    {
        let cb_rect = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " [{}] {}   (Space)",
                    if cb.checked { "x" } else { " " },
                    truncate_to_width(&cb.label, inner_w.saturating_sub(15))
                ),
                Style::default().fg(ctx.theme.fg_primary),
            )),
            cb_rect,
        );
        ctx.hit_registry
            .register(cb_rect, z + 2, HitTarget::DialogCheckbox);
        y += 2;
    }
    if d.radio {
        for (i, o) in d.options.iter().enumerate() {
            if y + 1 >= inner.bottom() {
                break;
            }
            let sel = i == d.selected;
            let bg = if sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let fg = if o.danger {
                ctx.theme.status_conflicted
            } else {
                ctx.theme.fg_bright
            };
            let mut spans = vec![Span::styled(
                format!(" {} ", if sel { "(•)" } else { "( )" }),
                Style::default().fg(ctx.theme.fg_accent_link),
            )];
            spans.extend(mnemonic_spans(
                &o.label,
                o.mnemonic,
                Style::default().fg(fg),
            ));
            if o.danger {
                spans.push(Span::styled(
                    ctx.t("dialog.reset.data_loss_badge").to_string(),
                    Style::default()
                        .fg(Color::White)
                        .bg(ctx.theme.status_conflicted)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let r = Rect::new(inner.x, y, inner.width, 1);
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
                r,
            );
            ctx.hit_registry
                .register(r, z + 2, HitTarget::DialogOption(i));
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(
                        "       {}",
                        truncate_to_width(&o.detail, inner_w.saturating_sub(7))
                    ),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
            y += 2;
        }
        let confirm_danger = d.options.get(d.selected).is_some_and(|o| o.danger);
        render_buttons(
            frame,
            ctx,
            inner.x + 1,
            inner.bottom().saturating_sub(1),
            inner.right(),
            &[
                DialogButtonSpec {
                    label: d.confirm_label.clone(),
                    selected: true,
                    enabled: true,
                    danger: confirm_danger,
                    target: HitTarget::DialogButton(DialogButtonKind::Confirm),
                },
                DialogButtonSpec {
                    label: ctx.cancel().into(),
                    selected: false,
                    enabled: true,
                    danger: false,
                    target: HitTarget::DialogButton(DialogButtonKind::Cancel),
                },
            ],
            z,
        );
        return;
    }
    // Button choices: one per line when they do not fit on a single row.
    let total: usize = d.options.iter().map(|o| o.label.width() + 4).sum();
    if total <= inner_w {
        let buttons: Vec<_> = d
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| DialogButtonSpec {
                label: o.label.clone(),
                selected: i == d.selected,
                enabled: true,
                danger: o.danger,
                target: HitTarget::DialogOption(i),
            })
            .collect();
        render_buttons(
            frame,
            ctx,
            inner.x + 1,
            inner.bottom().saturating_sub(1),
            inner.right(),
            &buttons,
            z,
        );
    } else {
        for (i, o) in d.options.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            let label = if o.detail.is_empty() {
                o.label.clone()
            } else {
                format!("{}  — {}", o.label, o.detail)
            };
            render_buttons(
                frame,
                ctx,
                inner.x + 1,
                y,
                inner.right(),
                &[DialogButtonSpec {
                    label,
                    selected: i == d.selected,
                    enabled: true,
                    danger: o.danger,
                    target: HitTarget::DialogOption(i),
                }],
                z,
            );
            y += 1;
        }
    }
}

fn render_message(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &MessageDialog,
    z: u8,
) {
    let rect = centered(screen, 78, 18);
    let inner = frame_block(frame, screen, ctx, rect, &d.title, false, z);
    let text_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(2),
    );
    let mut lines: Vec<Line<'_>> = d
        .text
        .split('\n')
        .enumerate()
        .map(|(i, l)| {
            let fg = if i == 0 {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_primary
            };
            Line::from(Span::styled(format!(" {l}"), Style::default().fg(fg)))
        })
        .collect();
    if let Some(last) = lines.last_mut() {
        last.spans.push(Span::styled(
            "▏",
            Style::default().fg(ctx.theme.border_focused),
        ));
    }
    let total = lines.len() as u16;
    let scroll = total.saturating_sub(text_area.height);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(Style::default().bg(ctx.theme.bg_panel)),
        text_area,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            ctx.t("dialog.message.hint").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
    );
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.ok().into(),
                selected: true,
                enabled: !d.text.trim().is_empty(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_rebase(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    d: &RebaseTodoDialog,
    z: u8,
) {
    let min_preview = if d.show_diff { 14u16 } else { 7u16 };
    let desired_h = (d.entries.len() as u16 + min_preview + 7).max(16);
    let h = desired_h.min(screen.height.saturating_sub(2));
    let w = if d.show_diff {
        screen.width.saturating_sub(6).min(112)
    } else {
        96
    };
    let rect = centered(screen, w, h);
    let base_str = d.base.map_or("--root".to_string(), |b| b.short());
    let title = ctx.t_fmt("dialog.rebase.title", &[("base", &base_str)]);
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let usable_h = inner.height.saturating_sub(3) as usize; // reserve hint + error + buttons
    let (list_h, preview_h) = if d.show_diff {
        let lh = (usable_h / 3).max(3).min(d.entries.len().max(1) + 1);
        (lh.saturating_sub(1), usable_h.saturating_sub(lh))
    } else {
        let ph = usable_h.clamp(3, 7).min(usable_h.saturating_sub(2));
        let lh = usable_h.saturating_sub(ph + 1);
        (lh, ph)
    };
    let scroll = if list_h > 0 && d.selected >= list_h {
        d.selected + 1 - list_h
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            ctx.t("dialog.rebase.columns").to_string(),
            Style::default()
                .fg(ctx.theme.fg_muted)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    for (vis, (i, e)) in d
        .entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(list_h)
        .enumerate()
    {
        let y = inner.y + 1 + vis as u16;
        let sel = i == d.selected;
        let color = match e.action {
            TodoAction::Pick => ctx.theme.fg_primary,
            TodoAction::Reword => ctx.theme.fg_accent_link,
            TodoAction::Edit => ctx.theme.status_added,
            TodoAction::Squash | TodoAction::Fixup => ctx.theme.status_modified,
            TodoAction::Drop => ctx.theme.status_conflicted,
            TodoAction::UpdateRef => ctx.theme.fg_muted,
        };
        let subject = e
            .message
            .as_deref()
            .filter(|_| e.action == TodoAction::Reword)
            .and_then(|m| m.lines().next())
            .unwrap_or(&e.subject);
        let mut subject_style = Style::default().fg(ctx.theme.fg_primary);
        if e.action == TodoAction::Drop {
            subject_style = subject_style
                .fg(ctx.theme.fg_muted)
                .add_modifier(Modifier::CROSSED_OUT);
        } else if e.action == TodoAction::UpdateRef {
            subject_style = Style::default()
                .fg(ctx.theme.fg_muted)
                .add_modifier(Modifier::ITALIC);
        }
        let act_lbl = ctx.todo_action_label(e.action);
        let act_pad = " ".repeat(11usize.saturating_sub(act_lbl.width()));
        let oid_col = if e.action == TodoAction::UpdateRef {
            "  (ref)  ".to_string()
        } else {
            format!(" {}  ", e.oid.short())
        };
        let line = Line::from(vec![
            Span::styled(
                format!(" {act_lbl}{act_pad}"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(oid_col, Style::default().fg(ctx.theme.fg_accent_link)),
            Span::styled(
                truncate_to_width(subject, inner.width.saturating_sub(24) as usize),
                subject_style,
            ),
        ]);
        let r = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(if sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            })),
            r,
        );
        ctx.hit_registry
            .register(r, z + 2, HitTarget::DialogOption(i));
    }

    // ── Bottom commit changed-files summary / full Diff preview ──
    let preview_y = inner.y + 1 + list_h as u16;
    if preview_h >= 2 && preview_y < inner.bottom().saturating_sub(2) {
        let preview_rect = Rect::new(inner.x, preview_y, inner.width, preview_h as u16);
        render_rebase_preview(frame, preview_rect, state, ctx, d);
    }

    let hint_y = inner.bottom().saturating_sub(2);
    let err = ctx.i18n.and_then(|i| d.error(i));
    let (hint, color) = match &err {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
        None if d.show_diff => (
            ctx.t("dialog.rebase.diff_hint").to_string(),
            ctx.theme.fg_muted,
        ),
        None => (
            ctx.t("dialog.rebase.hint").to_string(),
            ctx.theme.fg_muted,
        ),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hint, inner.width as usize),
            Style::default().fg(color),
        )),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: ctx.t("dialog.rebase.start_btn").into(),
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}

fn render_rebase_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    d: &RebaseTodoDialog,
) {
    let w = area.width as usize;
    let sep = "─".repeat(w);
    let Some(details_key) = d.desired_details_key() else {
        frame.render_widget(
            Paragraph::new(Span::styled(sep, Style::default().fg(ctx.theme.border_splitter))),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height >= 2 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {}", ctx.t("dialog.rebase.update_ref_readonly")),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
        return;
    };
    let Some(bundle) = state.diff_caches.details.peek(&details_key) else {
        frame.render_widget(
            Paragraph::new(Span::styled(sep, Style::default().fg(ctx.theme.border_splitter))),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height >= 2 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {}", ctx.t("dialog.rebase.preview_loading")),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
        return;
    };

    let count_s = bundle.changes.len().to_string();
    let header_text = if d.show_diff {
        let sel_idx = d.selected_file.min(bundle.changes.len().saturating_sub(1));
        if let Some(ch) = bundle.changes.get(sel_idx) {
            let idx_s = (sel_idx + 1).to_string();
            ctx.t_fmt(
                "dialog.rebase.diff_header",
                &[("idx", &idx_s), ("count", &count_s), ("path", &ch.path)],
            )
        } else {
            ctx.t("dialog.rebase.preview_empty").to_string()
        }
    } else {
        ctx.t_fmt("dialog.rebase.preview_header", &[("count", &count_s)])
    };
    let header_line = Line::from(vec![
        Span::styled(
            format!(" {header_text} "),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "─".repeat(w.saturating_sub(header_text.width() + 2)),
            Style::default().fg(ctx.theme.border_splitter),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header_line),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let body_h = area.height.saturating_sub(1) as usize;
    if body_h == 0 {
        return;
    }

    if bundle.changes.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("dialog.rebase.preview_empty")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        return;
    }

    let sel_file = d.selected_file.min(bundle.changes.len() - 1);
    if !d.show_diff {
        let start = if sel_file >= body_h {
            sel_file + 1 - body_h
        } else {
            0
        };
        for (vis, (i, ch)) in bundle
            .changes
            .iter()
            .enumerate()
            .skip(start)
            .take(body_h)
            .enumerate()
        {
            let y = area.y + 1 + vis as u16;
            let (badge, col) = match ch.kind {
                FileStatusKind::Added => ("A", ctx.theme.status_added),
                FileStatusKind::Deleted => ("D", ctx.theme.status_deleted),
                FileStatusKind::Renamed => ("R", ctx.theme.status_renamed),
                FileStatusKind::Copied => ("C", ctx.theme.status_renamed),
                _ => ("M", ctx.theme.status_modified),
            };
            let stats = format!(" +{} -{}", ch.additions, ch.deletions);
            let path_w = w.saturating_sub(stats.width() + 5);
            let is_sel = i == sel_file;
            let line = Line::from(vec![
                Span::styled(
                    format!(" {badge} "),
                    Style::default().fg(col).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_to_width(&ch.path, path_w),
                    Style::default().fg(ctx.theme.fg_primary),
                ),
                Span::styled(stats, Style::default().fg(ctx.theme.fg_muted)),
            ]);
            frame.render_widget(
                Paragraph::new(line).style(Style::default().bg(if is_sel {
                    ctx.theme.bg_panel
                } else {
                    ctx.theme.bg_elevated
                })),
                Rect::new(area.x, y, area.width, 1),
            );
        }
        return;
    }

    // Full unified diff mode for the selected file.
    let change = &bundle.changes[sel_file];
    let diff_key = FileDiffKey {
        details: details_key,
        path: change.path.clone(),
        old_path: change.old_path.clone(),
        context: DiffContext::from_lines(state.config.jbgit.diff.context_lines),
        whitespace: state.config.jbgit.diff.whitespace.to_core(),
    };
    let Some(diff) = state.diff_caches.peek_diff(&diff_key) else {
        ctx.record_scroll_bound(ScrollPane::RebaseDiff, body_h, 0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("dialog.rebase.preview_loading")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        return;
    };

    let mut diff_lines: Vec<Line<'static>> = Vec::new();
    if diff.is_binary {
        diff_lines.push(Line::from(Span::styled(
            format!(" {}", ctx.t("diff.binary_file")),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    } else if diff.hunks.is_empty() {
        diff_lines.push(Line::from(Span::styled(
            format!(" {}", ctx.t("diff.no_changes")),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    } else {
        for hunk in &diff.hunks {
            diff_lines.push(Line::from(Span::styled(
                truncate_to_width(&hunk.header, w),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_panel),
            )));
            for dl in &hunk.lines {
                let (prefix, fg) = match dl.kind {
                    DiffLineKind::Added => ('+', ctx.theme.status_added),
                    DiffLineKind::Deleted => ('-', ctx.theme.status_deleted),
                    DiffLineKind::Context => (' ', ctx.theme.fg_primary),
                    DiffLineKind::NoNewline => ('\\', ctx.theme.fg_muted),
                };
                let text = format!("{prefix}{}", dl.text);
                diff_lines.push(Line::from(Span::styled(
                    truncate_to_width(&text, w),
                    Style::default().fg(fg),
                )));
            }
        }
    }
    let total = diff_lines.len();
    let max_scroll = total.saturating_sub(body_h);
    ctx.record_scroll_bound(ScrollPane::RebaseDiff, body_h, max_scroll);
    let offset = d.diff_scroll.offset().min(max_scroll);
    for (vis, line) in diff_lines.into_iter().skip(offset).take(body_h).enumerate() {
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, area.y + 1 + vis as u16, area.width, 1),
        );
    }
}

fn render_create_patch(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &CreatePatchDialog,
    z: u8,
) {
    let rect = centered(screen, 84, 12);
    let title = ctx.t("dialog.create_patch.title").to_string();
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let width = inner.width as usize;
    let muted = Style::default().fg(ctx.theme.fg_muted);

    // Row 0: Destination (File vs Clipboard)
    let dest_sel = d.row() == CreatePatchRow::Destination;
    let dest_bg = if dest_sel {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_elevated
    };
    let file_radio = if !d.to_clipboard { "●" } else { "○" };
    let clip_radio = if d.to_clipboard { "●" } else { "○" };
    let dest_line = Line::from(vec![
        Span::styled(
            format!(" {}: ", ctx.t("dialog.create_patch.destination")),
            muted.bg(dest_bg),
        ),
        Span::styled(
            format!(
                "{file_radio} {}   {clip_radio} {}",
                ctx.t("dialog.create_patch.to_file"),
                ctx.t("dialog.create_patch.to_clipboard")
            ),
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(dest_bg),
        ),
    ]);
    let r0 = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    frame.render_widget(Paragraph::new(dest_line).style(Style::default().bg(dest_bg)), r0);
    ctx.hit_registry
        .register(r0, z + 2, HitTarget::DialogOption(0));

    // Row 1: Output path (enabled when File)
    let path_sel = d.row() == CreatePatchRow::Path;
    let path_bg = if path_sel && !d.to_clipboard {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_elevated
    };
    let cursor = if path_sel && !d.to_clipboard { "▏" } else { "" };
    let path_fg = if d.to_clipboard {
        ctx.theme.fg_muted
    } else {
        ctx.theme.fg_primary
    };
    let path_lbl = format!(" {}: ", ctx.t("dialog.create_patch.path"));
    let path_avail = width.saturating_sub(path_lbl.width() + 2);
    let shown_path = if d.path.width() > path_avail && path_avail > 2 {
        let mut chars: Vec<char> = d.path.chars().collect();
        while chars.iter().collect::<String>().width() + 1 > path_avail && !chars.is_empty() {
            chars.remove(0);
        }
        format!("…{}", chars.into_iter().collect::<String>())
    } else {
        d.path.clone()
    };
    let r1 = Rect::new(inner.x, inner.y + 3, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(path_lbl, muted.bg(path_bg)),
            Span::styled(format!("{shown_path}{cursor}"), Style::default().fg(path_fg).bg(path_bg)),
        ]))
        .style(Style::default().bg(path_bg)),
        r1,
    );
    ctx.hit_registry
        .register(r1, z + 2, HitTarget::DialogOption(1));

    // Row 2: Reverse (-R)
    let rev_sel = d.row() == CreatePatchRow::Reverse;
    let rev_bg = if rev_sel {
        ctx.theme.bg_selection_focused
    } else {
        ctx.theme.bg_elevated
    };
    let rev_box = if d.reverse { "[x]" } else { "[ ]" };
    let r2 = Rect::new(inner.x, inner.y + 5, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {rev_box} {}", ctx.t("dialog.create_patch.reverse")),
            Style::default()
                .fg(if d.reverse {
                    ctx.theme.fg_accent_link
                } else {
                    ctx.theme.fg_primary
                })
                .bg(rev_bg),
        )))
        .style(Style::default().bg(rev_bg)),
        r2,
    );
    ctx.hit_registry
        .register(r2, z + 2, HitTarget::DialogOption(2));

    // Error / hint line.
    let err = ctx.i18n.and_then(|i| d.error(i));
    let (hint, hint_fg) = match &err {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
        None => (
            format!(" {}", ctx.t("dialog.create_patch.hint")),
            ctx.theme.fg_muted,
        ),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&hint, width),
            Style::default().fg(hint_fg),
        )),
        Rect::new(inner.x, inner.bottom().saturating_sub(3), inner.width, 1),
    );

    let confirm_lbl = if d.to_clipboard {
        ctx.t("dialog.create_patch.copy_btn").into()
    } else {
        ctx.t("dialog.create_patch.create_btn").into()
    };
    render_buttons(
        frame,
        ctx,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.right(),
        &[
            DialogButtonSpec {
                label: confirm_lbl,
                selected: true,
                enabled: err.is_none(),
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Confirm),
            },
            DialogButtonSpec {
                label: ctx.cancel().into(),
                selected: false,
                enabled: true,
                danger: false,
                target: HitTarget::DialogButton(DialogButtonKind::Cancel),
            },
        ],
        z,
    );
}
