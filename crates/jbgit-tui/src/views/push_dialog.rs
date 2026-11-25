//! Full Push dialog modal: three-column preview (`Target | Outgoing Commits | Changed Files / Diff`)
//! plus bottom options (`Push tags`, `Force`, `Set upstream`), protected-branch guards, and
//! inline target editing with remote branch completion.

use jbgit_core::{FileStatusKind, PushForce, PushTagMode};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::{HitTarget, PushActionButtonKind, PushOptionKind};
use crate::log_view::{PushDialogFocus, PushDialogState};
use crate::views::log_details::{FileDiffRenderProps, render_file_diff_body};
use crate::views::overlays::draw_shadow;
use crate::views::{fit_left_right_spans, list_viewport, row_background, truncate_to_width};

pub fn render_push_dialog_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    pd: &PushDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 24 || screen.height < 12 {
        return;
    }

    let width = if screen.width >= 40 {
        ((u32::from(screen.width) * 92) / 100).max(36) as u16
    } else {
        screen.width.saturating_sub(2).max(24)
    }
    .min(screen.width);
    let height = if screen.height >= 16 {
        ((u32::from(screen.height) * 86) / 100).max(14) as u16
    } else {
        screen.height.saturating_sub(2).max(10)
    }
    .min(screen.height);

    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );

    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let target_name = pd.remote_branch.trim();
    let title = ctx.t_fmt(
        "dialog.push_full.title",
        &[
            ("branch", &pd.branch),
            ("remote", pd.remote()),
            ("target", target_name),
        ],
    );
    let is_force = pd.force.is_force();
    let block = Block::default()
        .title(Span::styled(
            format!(
                " {} ",
                truncate_to_width(&title, rect.width.saturating_sub(6) as usize)
            ),
            Style::default()
                .fg(if is_force {
                    ctx.theme.status_conflicted
                } else {
                    ctx.theme.fg_bright
                })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if is_force {
            ctx.theme.status_conflicted
        } else {
            ctx.theme.border_focused
        }))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    if inner.height < 6 || inner.width < 16 {
        return;
    }

    let bottom_h = 3u16.min(inner.height.saturating_sub(2));
    let body_rect = Rect::new(inner.x, inner.y, inner.width, inner.height - bottom_h);
    let bottom_rect = Rect::new(inner.x, body_rect.bottom(), inner.width, bottom_h);

    if body_rect.width >= 90 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(28),
                Constraint::Percentage(38),
                Constraint::Percentage(34),
            ])
            .split(body_rect);
        render_target_pane(frame, cols[0], state, pd, ctx, z + 2);
        render_commits_pane(frame, cols[1], state, pd, ctx, z + 2);
        render_files_pane(frame, cols[2], state, pd, ctx, z + 2);
    } else if body_rect.height >= 12 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Percentage(55),
                Constraint::Min(3),
            ])
            .split(body_rect);
        render_target_pane(frame, rows[0], state, pd, ctx, z + 2);
        render_commits_pane(frame, rows[1], state, pd, ctx, z + 2);
        render_files_pane(frame, rows[2], state, pd, ctx, z + 2);
    } else {
        render_commits_pane(frame, body_rect, state, pd, ctx, z + 2);
    }

    render_bottom_bar(frame, bottom_rect, state, pd, ctx, z + 2);
}

fn pane_block<'a>(ctx: &RenderContext<'_>, title: &str, focused: bool) -> Block<'a> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_muted
                })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if focused {
            ctx.theme.border_focused
        } else {
            ctx.theme.border_splitter
        }))
        .style(Style::default().bg(ctx.theme.bg_panel))
}

fn render_target_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    pd: &PushDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    let focused = pd.focus == PushDialogFocus::Target || pd.editing_target || pd.picking_remote;
    let block = pane_block(ctx, ctx.t("dialog.push_full.pane_target"), focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 6 || inner.height < 2 {
        return;
    }

    let w = inner.width as usize;
    let mut y = inner.y;

    // Row 0: source branch -> remote (or inline [+ Define Remote…])
    let has_remote = !pd.remotes.is_empty();
    let remote_badge = if pd.outgoing.as_deref().is_some_and(|b| b.remote_exists) {
        String::new()
    } else {
        ctx.t("dialog.push_full.new_remote_badge").to_string()
    };
    let remote_span = if has_remote {
        Span::styled(
            format!("[{} ▾]", pd.remote()),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            ctx.t("dialog.push_full.define_remote_btn").to_string(),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD),
        )
    };
    let src_line = Line::from(vec![
        Span::styled(
            truncate_to_width(&pd.branch, (w / 2).max(4)),
            Style::default()
                .fg(if pd.is_detached() {
                    ctx.theme.ref_detached.icon
                } else {
                    ctx.theme.ref_local.icon
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" → ", Style::default().fg(ctx.theme.fg_muted)),
        remote_span,
    ]);
    let row0 = Rect::new(inner.x, y, inner.width, 1);
    frame.render_widget(Paragraph::new(src_line), row0);
    ctx.hit_registry.register(
        row0,
        z + 1,
        if has_remote {
            HitTarget::PushRemotePicker
        } else {
            HitTarget::PushDefineRemote
        },
    );
    y += 1;
    if y >= inner.bottom() {
        return;
    }

    // Row 1: target branch (inline edit or clickable badge)
    let row1 = Rect::new(inner.x, y, inner.width, 1);
    if pd.editing_target {
        let input_text = format!(
            " : {}▏",
            truncate_to_width(&pd.target_draft, w.saturating_sub(6))
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    "{input_text}{}",
                    " ".repeat(w.saturating_sub(input_text.width()))
                ),
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.bg_selection_focused),
            )),
            row1,
        );
    } else {
        let shown_target = if pd.remote_branch.trim().is_empty() {
            ctx.t("dialog.push_full.target_placeholder")
        } else {
            pd.remote_branch.as_str()
        };
        let target_text = format!(
            " : {} ✎{remote_badge}",
            truncate_to_width(shown_target, w.saturating_sub(10))
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                target_text,
                Style::default().fg(if pd.remote_branch.trim().is_empty() {
                    ctx.theme.fg_muted
                } else {
                    ctx.theme.fg_bright
                }),
            )),
            row1,
        );
        ctx.hit_registry
            .register(row1, z + 1, HitTarget::PushTargetBranch);
    }
    y += 1;

    // Optional picker/completion list or summary stats
    if pd.picking_remote {
        for (idx, rm) in pd.remotes.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            let sel = idx == pd.remote_idx;
            let r = Rect::new(inner.x, y, inner.width, 1);
            let marker = if sel { "▸" } else { " " };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {marker} {}", truncate_to_width(rm, w.saturating_sub(4))),
                    Style::default()
                        .fg(if sel {
                            ctx.theme.fg_bright
                        } else {
                            ctx.theme.fg_primary
                        })
                        .bg(if sel {
                            ctx.theme.bg_selection_focused
                        } else {
                            ctx.theme.bg_panel
                        }),
                )),
                r,
            );
            ctx.hit_registry
                .register(r, z + 2, HitTarget::PushRemoteOption(idx));
            y += 1;
        }
        return;
    }

    if pd.editing_target {
        if let Some(err) = pd.target_validation_error(&state.i18n)
            && y < inner.bottom()
        {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" ✕ {}", truncate_to_width(&err, w.saturating_sub(4))),
                    Style::default().fg(ctx.theme.status_conflicted),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
        let cands = pd.completion_candidates(state.repo_snapshot.as_deref());
        let avail = inner.bottom().saturating_sub(y) as usize;
        let (sel, scroll) = list_viewport(cands.len(), pd.completion_selected, avail);
        for (vis, (idx, cand)) in cands
            .iter()
            .enumerate()
            .skip(scroll)
            .take(avail)
            .enumerate()
        {
            let row_y = y + vis as u16;
            let r = Rect::new(inner.x, row_y, inner.width, 1);
            let is_sel = idx == sel;
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("   {}", truncate_to_width(cand, w.saturating_sub(4))),
                    Style::default()
                        .fg(if is_sel {
                            ctx.theme.fg_bright
                        } else {
                            ctx.theme.fg_primary
                        })
                        .bg(if is_sel {
                            ctx.theme.bg_selection_focused
                        } else {
                            ctx.theme.bg_panel
                        }),
                )),
                r,
            );
            ctx.hit_registry
                .register(r, z + 2, HitTarget::PushCompletionItem(idx));
        }
        return;
    }

    if y < inner.bottom() {
        y += 1;
    }
    if y < inner.bottom() {
        let stats = if pd.loading {
            ctx.t("dialog.push_full.loading_outgoing").to_string()
        } else if let Some(b) = pd.outgoing.as_deref() {
            let commits_n = pd.visible_commits().len();
            let c_s = if b.truncated {
                format!("{commits_n}+")
            } else {
                commits_n.to_string()
            };
            let f_s = b.files_changed.to_string();
            ctx.t_fmt(
                "dialog.push_full.summary_stats",
                &[("commits", &c_s), ("files", &f_s)],
            )
        } else {
            String::new()
        };
        if !stats.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {}", truncate_to_width(&stats, w.saturating_sub(2))),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
        }
    }
    if let Some(oid) = pd.up_to
        && y < inner.bottom()
    {
        let short = oid.short();
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " {}",
                    truncate_to_width(
                        &ctx.t_fmt("dialog.push_full.up_to_badge", &[("oid", &short)]),
                        w.saturating_sub(2)
                    )
                ),
                Style::default().fg(ctx.theme.ref_detached.icon),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
}

fn render_commits_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    pd: &PushDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    let focused = pd.focus == PushDialogFocus::Commits && !pd.editing_target && !pd.picking_remote;
    let commits = pd.visible_commits();
    let count_s = if pd.outgoing.as_deref().is_some_and(|b| b.truncated) {
        format!("{}+", commits.len())
    } else {
        commits.len().to_string()
    };
    let title = ctx.t_fmt("dialog.push_full.pane_commits", &[("count", &count_s)]);
    let block = pane_block(ctx, &title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    ctx.hit_registry
        .register(area, z, HitTarget::PushCommitListArea);
    if inner.width < 6 || inner.height == 0 {
        return;
    }

    if pd.loading {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " {} {}",
                    state.spinner_char(),
                    ctx.t("dialog.push_full.loading_outgoing")
                ),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    }
    if let Some(err) = &pd.error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", truncate_to_width(err, inner.width as usize)),
                Style::default().fg(ctx.theme.status_conflicted),
            )),
            inner,
        );
        return;
    }
    if commits.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("dialog.push_full.no_outgoing")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    }

    let now_ts = chrono::Local::now().timestamp();
    let h = inner.height as usize;
    let selected = pd.selected_commit.min(commits.len() - 1);
    let mut vp = pd.commit_scroll;
    vp.ensure_visible(selected, commits.len(), h);
    let scroll = vp.offset();
    ctx.record_scroll_bound(ScrollPane::PushCommits, h, commits.len().saturating_sub(h));
    let w = inner.width as usize;

    for (vis, (idx, c)) in commits.iter().enumerate().skip(scroll).take(h).enumerate() {
        let y = inner.y + vis as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_sel = idx == selected;
        let bg = row_background(is_sel, focused, false, ctx.theme.bg_panel, ctx.theme);
        let short = c.oid.short();
        let rel = ctx.format_relative_time(c.timestamp, now_ts);
        let right = vec![
            Span::styled(
                format!("{} ", truncate_to_width(&c.author_name, 14)),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(format!("{rel} "), Style::default().fg(ctx.theme.fg_muted)),
        ];
        let right_w: usize = right.iter().map(|s| s.content.width()).sum();
        let subj_w = w.saturating_sub(short.width() + right_w + 3);
        let left = vec![
            Span::styled(
                format!(" {short} "),
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
            Span::styled(
                truncate_to_width(&c.subject, subj_w),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
        ];
        let spans = fit_left_right_spans(left, right, w, 0);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 1, HitTarget::PushCommitRow(idx));
    }
}

fn render_files_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    pd: &PushDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    let focused = pd.focus == PushDialogFocus::Files && !pd.editing_target && !pd.picking_remote;
    let changes = pd.selected_file_changes(&state.diff_caches);
    let title = if pd.show_diff {
        pd.selected_file_change(&state.diff_caches)
            .map(|c| c.path.as_str())
            .unwrap_or_else(|| ctx.t("dialog.push_full.pane_diff"))
    } else {
        ctx.t("dialog.push_full.pane_files")
    };
    let block = pane_block(ctx, title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if pd.show_diff {
        ctx.hit_registry.register(area, z, HitTarget::PushDiffArea);
        if inner.width < 6 || inner.height < 2 {
            return;
        }
        let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
        let body_rect = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
        let back_lbl = ctx.t("dialog.push_full.back_to_files");
        let back_w = (back_lbl.width() as u16 + 2).min(inner.width);
        let back_rect = Rect::new(header_rect.x, header_rect.y, back_w, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {back_lbl} "),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_elevated),
            )),
            back_rect,
        );
        ctx.hit_registry
            .register(back_rect, z + 2, HitTarget::PushDiffBackButton);
        if let Some(key) = pd.desired_diff_key(&state.diff_caches)
            && let Some(diff) = state.diff_caches.peek_diff(&key)
        {
            let cached_syntax = state.diff_caches.peek_syntax(&key).map(|s| s.as_ref());
            render_file_diff_body(
                frame,
                body_rect,
                FileDiffRenderProps {
                    diff,
                    session: &pd.diff,
                    cached_syntax,
                    scroll_pane: ScrollPane::PushDiff,
                },
                ctx,
            );
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(
                        " {} {}",
                        state.spinner_char(),
                        ctx.t("view.log_details.loading_diff")
                    ),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                body_rect,
            );
        }
        return;
    }

    ctx.hit_registry
        .register(area, z, HitTarget::PushFileListArea);
    if inner.width < 6 || inner.height == 0 {
        return;
    }
    if pd.selected_commit_summary().is_none() {
        return;
    }
    let Some(details_key) = pd.desired_details_key() else {
        return;
    };
    if !state.diff_caches.details.contains(&details_key) {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " {} {}",
                    state.spinner_char(),
                    ctx.t("view.log_details.loading_changes").trim_start()
                ),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    }
    if changes.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", ctx.t("view.log_details.no_files")),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    }

    let h = inner.height as usize;
    let selected = pd.selected_file.min(changes.len() - 1);
    let mut vp = pd.file_scroll;
    vp.ensure_visible(selected, changes.len(), h);
    let scroll = vp.offset();
    ctx.record_scroll_bound(ScrollPane::PushFiles, h, changes.len().saturating_sub(h));
    let w = inner.width as usize;

    for (vis, (idx, ch)) in changes.iter().enumerate().skip(scroll).take(h).enumerate() {
        let y = inner.y + vis as u16;
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_sel = idx == selected;
        let bg = row_background(is_sel, focused, false, ctx.theme.bg_panel, ctx.theme);
        let (badge, color) = match ch.kind {
            FileStatusKind::Added => ("A", ctx.theme.status_added),
            FileStatusKind::Deleted => ("D", ctx.theme.status_deleted),
            FileStatusKind::Renamed => ("R", ctx.theme.status_renamed),
            FileStatusKind::Copied => ("C", ctx.theme.status_renamed),
            FileStatusKind::Conflicted => ("!", ctx.theme.status_conflicted),
            _ => ("M", ctx.theme.status_modified),
        };
        let stat = if ch.is_binary {
            "bin".to_string()
        } else {
            format!("+{} -{}", ch.additions, ch.deletions)
        };
        let right = vec![Span::styled(
            format!("{stat} "),
            Style::default().fg(ctx.theme.fg_muted),
        )];
        let left = vec![
            Span::styled(
                format!(" {badge} "),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_to_width(&ch.path, w.saturating_sub(stat.width() + 6)),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
        ];
        let spans = fit_left_right_spans(left, right, w, 0);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, z + 1, HitTarget::PushFileRow(idx));
    }
}

fn render_bottom_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    pd: &PushDialogState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if area.height == 0 || area.width < 12 {
        return;
    }

    // Row 0: options bar
    let tags_lbl = match pd.tags {
        PushTagMode::None => ctx.t("dialog.push_full.tags_none"),
        PushTagMode::Follow => ctx.t("dialog.push_full.tags_follow"),
        PushTagMode::All => ctx.t("dialog.push_full.tags_all"),
    };
    let force_lbl = match pd.force {
        PushForce::None => ctx.t("dialog.push_full.force_none"),
        PushForce::WithLease => ctx.t("dialog.push_full.force_with_lease"),
        PushForce::Force => ctx.t("dialog.push_full.force_yes"),
    };
    let upstream_check = if pd.is_detached() {
        "[-]"
    } else if pd.set_upstream {
        "[x]"
    } else {
        "[ ]"
    };
    let no_verify_check = if pd.no_verify { "[x]" } else { "[ ]" };
    let pills = [
        (
            format!(" [t] {}: {tags_lbl} ", ctx.t("dialog.push_full.opt_tags")),
            pd.tags != PushTagMode::None,
            false,
            false,
            HitTarget::PushOption(PushOptionKind::Tags),
        ),
        (
            format!(" [f] {}: {force_lbl} ", ctx.t("dialog.push_full.opt_force")),
            pd.force.is_force(),
            pd.force.is_force(),
            false,
            HitTarget::PushOption(PushOptionKind::Force),
        ),
        (
            format!(
                " [u] {upstream_check} {} ",
                ctx.t("dialog.push_full.opt_upstream")
            ),
            !pd.is_detached() && pd.set_upstream,
            false,
            pd.is_detached(),
            HitTarget::PushOption(PushOptionKind::SetUpstream),
        ),
        (
            format!(
                " [n] {no_verify_check} {} ",
                ctx.t("dialog.push_full.opt_no_verify")
            ),
            pd.no_verify,
            false,
            false,
            HitTarget::PushOption(PushOptionKind::NoVerify),
        ),
    ];
    let mut cx = area.x;
    for (text, active, danger, disabled, target) in pills {
        let w = text.width() as u16;
        if cx + w > area.right() {
            break;
        }
        let style = if disabled {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::DIM)
        } else if danger {
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD)
        } else if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        let r = Rect::new(cx, area.y, w, 1);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), r);
        ctx.hit_registry.register(r, z + 1, target);
        cx += w + 1;
    }

    // Row 1: status / protected warning / hint
    if area.height >= 2 {
        let protected = &state.config.jbgit.push.protected_branches;
        let (msg, color) = if pd.is_force_blocked(protected) {
            (
                format!(
                    " ✕ {}",
                    ctx.t_fmt(
                        "dialog.push_full.protected_force_blocked",
                        &[("branch", pd.remote_branch.trim())]
                    )
                ),
                ctx.theme.status_conflicted,
            )
        } else if let Some(err) = pd.target_validation_error(&state.i18n) {
            (format!(" ✕ {err}"), ctx.theme.status_conflicted)
        } else if pd.force.is_force() {
            (
                format!(" ⚠ {}", ctx.t("dialog.push_full.force_warn")),
                ctx.theme.ref_detached.icon,
            )
        } else {
            (
                format!(" {}", ctx.t("dialog.push_full.hint")),
                ctx.theme.fg_muted,
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&msg, area.width as usize),
                Style::default().fg(color),
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }

    // Row 2: [Push] [Cancel]
    if area.height >= 3 {
        let protected = &state.config.jbgit.push.protected_branches;
        let can = pd.can_push(protected, &state.i18n).is_ok();
        let push_label = match pd.force {
            PushForce::None => ctx.t("dialog.push.push_btn").to_string(),
            PushForce::WithLease => ctx.t("dialog.push.force_push_btn").to_string(),
            PushForce::Force => ctx.t("dialog.push_full.force_btn").to_string(),
        };
        let btn_y = area.y + 2;
        let mut bx = area.x + 1;
        let push_text = format!(" {push_label} ");
        let push_w = push_text.width() as u16;
        if bx + push_w <= area.right() {
            let push_style = if !can {
                Style::default()
                    .fg(ctx.theme.fg_muted)
                    .bg(ctx.theme.bg_panel)
            } else if pd.force.is_force() {
                Style::default()
                    .fg(Color::White)
                    .bg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.border_focused)
                    .add_modifier(Modifier::BOLD)
            };
            let pr = Rect::new(bx, btn_y, push_w, 1);
            frame.render_widget(Paragraph::new(Span::styled(push_text, push_style)), pr);
            ctx.hit_registry.register(
                pr,
                z + 1,
                HitTarget::PushActionButton(PushActionButtonKind::Confirm),
            );
            bx += push_w + 2;
        }
        let cancel_text = format!(" {} ", ctx.cancel());
        let cancel_w = cancel_text.width() as u16;
        if bx + cancel_w <= area.right() {
            let cr = Rect::new(bx, btn_y, cancel_w, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    cancel_text,
                    Style::default()
                        .fg(ctx.theme.fg_primary)
                        .bg(ctx.theme.bg_panel),
                )),
                cr,
            );
            ctx.hit_registry.register(
                cr,
                z + 1,
                HitTarget::PushActionButton(PushActionButtonKind::Cancel),
            );
        }
    }
}
