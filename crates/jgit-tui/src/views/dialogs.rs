//! Rendering of [`Dialog`] layers: validated input, radio / button choice,
//! multi-line message editor and the interactive-rebase todo table.

use jgit_core::TodoAction;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::RenderContext;
use crate::dialogs::{ChoiceDialog, Dialog, InputDialog, MessageDialog, RebaseTodoDialog};
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
        let text = format!(" {} ", b.label);
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
    ctx: &mut RenderContext<'_>,
    dialog: &Dialog,
    z: u8,
) {
    if screen.width < 20 || screen.height < 8 {
        return;
    }
    match dialog {
        Dialog::Input(d) => render_input(frame, screen, ctx, d, z),
        Dialog::Choice(d) => render_choice(frame, screen, ctx, d, z),
        Dialog::Message(d) => render_message(frame, screen, ctx, d, z),
        Dialog::RebaseTodo(d) => render_rebase(frame, screen, ctx, d, z),
    }
}

fn render_input(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    d: &InputDialog,
    z: u8,
) {
    let h = 8 + u16::from(d.checkbox.is_some());
    let rect = centered(screen, 64, h);
    let inner = frame_block(frame, screen, ctx, rect, &d.title, false, z);
    let w = inner.width as usize;
    let err = d.error();
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
    if let Some((label, on)) = &d.checkbox {
        lines.push(Line::from(Span::styled(
            format!(" [{}] {label}   (Tab)", if *on { "x" } else { " " }),
            Style::default().fg(ctx.theme.fg_primary),
        )));
        ctx.hit_registry.register(
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
            z + 2,
            HitTarget::DialogCheckbox,
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
    let h = (body.len() + opt_rows + 5) as u16;
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
    ctx: &mut RenderContext<'_>,
    d: &RebaseTodoDialog,
    z: u8,
) {
    let h = (d.entries.len() as u16 + 7).min(screen.height.saturating_sub(2));
    let rect = centered(screen, 92, h);
    let base_str = d.base.map_or("--root".to_string(), |b| b.short());
    let title = ctx.t_fmt("dialog.rebase.title", &[("base", &base_str)]);
    let inner = frame_block(frame, screen, ctx, rect, &title, false, z);
    let list_h = inner.height.saturating_sub(4) as usize;
    let scroll = if d.selected >= list_h {
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
            TodoAction::Squash | TodoAction::Fixup => ctx.theme.status_modified,
            TodoAction::Drop => ctx.theme.status_conflicted,
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
        }
        let line = Line::from(vec![
            Span::styled(
                format!(" {:<8}", e.action.label()),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}  ", e.oid.short()),
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
            Span::styled(
                truncate_to_width(subject, inner.width.saturating_sub(22) as usize),
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
    let hint_y = inner.bottom().saturating_sub(3);
    let (hint, color) = match d.error() {
        Some(e) => (format!(" ✕ {e}"), ctx.theme.status_conflicted),
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
        Rect::new(inner.x, hint_y + 1, inner.width, 1),
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
                enabled: d.error().is_none(),
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
