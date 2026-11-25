use jbgit_core::word_diff;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, PopupKind, RenderContext};
use crate::hit_test::HitTarget;
use crate::log_view::FilterField;
use crate::syntax::render_diff_line_spans_with_tab;
use crate::views::palette_preview::render_palette_preview;
use crate::views::truncate_to_width;

use super::truncate_display_width;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_floating_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    kind: PopupKind,
    query: &str,
    selected: usize,
    selection_touched: bool,
    z: u8,
) {
    if kind == PopupKind::BranchesQuickView {
        crate::branches_popup::render_branches_popup(frame, screen, state, ctx, query, selected, z);
        return;
    }
    let (max_w, max_h) = match kind {
        PopupKind::PalettePreview => (100, 38),
        PopupKind::ShortcutsHelp => (90, 38),
        PopupKind::GoToRevision => (64, 7),
        PopupKind::ChooseParent | PopupKind::ChooseChild => (84, 14),
        PopupKind::CommitHistory => (76, 18),
        _ => (64, 18),
    };
    let title = match kind {
        PopupKind::ShortcutsHelp => {
            let t = ctx.t("help.shortcuts_title");
            if t == "help.shortcuts_title" {
                " Keyboard & Mouse Shortcuts (Esc to close) ".to_string()
            } else {
                format!(" {t} ")
            }
        }
        PopupKind::BranchesQuickView => format!(" {} ", ctx.t("view.branches_popup.title")),
        PopupKind::PalettePreview => ctx.t("popup.palette_title").to_string(),
        PopupKind::FilterEditor(field) => {
            let field_name = ctx
                .i18n
                .map_or(field.title(), |i| i.filter_field_title(field));
            ctx.t_fmt("popup.filter_by", &[("field", field_name)])
        }
        PopupKind::GoToRevision => ctx.t("popup.goto_title").to_string(),
        PopupKind::ChooseParent => ctx.t("popup.choose_parent_title").to_string(),
        PopupKind::ChooseChild => ctx.t("popup.choose_child_title").to_string(),
        PopupKind::CommitHistory => ctx.t("popup.commit_history_title").to_string(),
    };

    let inner = render_centered_popup_box(frame, screen, ctx, max_w, max_h, title, z);

    match kind {
        PopupKind::PalettePreview => render_palette_preview(frame, inner, ctx),
        PopupKind::ShortcutsHelp => {
            render_shortcuts_help_popup(frame, inner, state, ctx, query, selected, z);
        }
        PopupKind::FilterEditor(field) => {
            render_filter_editor_popup(
                frame,
                inner,
                state,
                ctx,
                field,
                query,
                selected,
                selection_touched,
            );
        }
        PopupKind::GoToRevision => render_goto_revision_popup(frame, inner, state, ctx, query),
        PopupKind::ChooseParent => {
            render_choose_adjacent_popup(frame, inner, state, ctx, true, selected);
        }
        PopupKind::ChooseChild => {
            render_choose_adjacent_popup(frame, inner, state, ctx, false, selected);
        }
        PopupKind::BranchesQuickView => {}
        PopupKind::CommitHistory => {
            render_commit_history_popup(frame, inner, state, ctx, query, selected);
        }
    }
}

fn render_centered_popup_box(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    max_w: u16,
    max_h: u16,
    title: String,
    z: u8,
) -> Rect {
    let width = screen.width.min(max_w);
    let height = screen.height.min(max_h);
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen
        .y
        .saturating_add(screen.height.saturating_sub(height) / 2);
    let rect = Rect::new(x, y, width, height);

    crate::views::clear_modal_overlay(frame, rect);
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);
    inner
}

pub(super) fn render_three_way_inspector_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    range_idx: Option<usize>,
    z: u8,
) {
    let title = ctx.t("popup.three_way_title").to_string();
    let inner = render_centered_popup_box(frame, screen, ctx, 84, 22, title, z);
    render_three_way_inspector_popup(frame, inner, state, ctx, range_idx);
}

pub(super) fn render_hook_error_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    output: &str,
    z: u8,
) {
    let title = ctx.t("popup.hook_error_title").to_string();
    let inner = render_centered_popup_box(frame, screen, ctx, 84, 20, title, z);
    render_hook_error_popup(frame, inner, ctx, output);
}

fn render_shortcuts_help_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    query: &str,
    selected: usize,
    z: u8,
) {
    if inner.width < 12 || inner.height < 4 {
        return;
    }
    let git_summary = state.git_executable_summary();
    let has_git_row = git_summary.is_some() && inner.height >= 6;
    let search_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let div_rect = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let list_h = inner.height.saturating_sub(if has_git_row { 4 } else { 3 });
    let list_rect = Rect::new(inner.x, inner.y + 2, inner.width, list_h);
    let git_rect = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(2),
        inner.width,
        1,
    );
    let foot_rect = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);

    let prompt_prefix = " > ";
    let esc_hint = " [Esc] ";
    let avail_query_w =
        (inner.width as usize).saturating_sub(prompt_prefix.width() + esc_hint.width() + 2);
    let mut search_spans: Vec<Span<'_>> = vec![Span::styled(
        prompt_prefix,
        Style::default()
            .fg(ctx.theme.fg_accent_link)
            .bg(ctx.theme.bg_panel)
            .add_modifier(Modifier::BOLD),
    )];
    let mut used_search_w = prompt_prefix.width();
    if query.is_empty() {
        let placeholder = truncate_display_width(ctx.t("help.search_placeholder"), avail_query_w);
        used_search_w += placeholder.width();
        search_spans.push(Span::styled(
            placeholder,
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel),
        ));
    } else {
        let q_disp = truncate_display_width(query, avail_query_w);
        used_search_w += q_disp.width() + 1;
        search_spans.push(Span::styled(
            q_disp,
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel),
        ));
        search_spans.push(Span::styled(
            "▏",
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel),
        ));
    }
    let search_gap = (inner.width as usize).saturating_sub(used_search_w + esc_hint.width());
    if search_gap > 0 {
        search_spans.push(Span::styled(
            " ".repeat(search_gap),
            Style::default().bg(ctx.theme.bg_panel),
        ));
    }
    search_spans.push(Span::styled(
        esc_hint,
        Style::default()
            .fg(ctx.theme.fg_muted)
            .bg(ctx.theme.bg_panel),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(search_spans)).style(Style::default().bg(ctx.theme.bg_panel)),
        search_rect,
    );
    crate::views::register_right_esc_cancel(ctx, search_rect, esc_hint, z + 2);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(ctx.theme.border_splitter),
        )))
        .style(Style::default().bg(ctx.theme.bg_elevated)),
        div_rect,
    );

    let rows = crate::keymap::build_help_rows(state, query);
    let resolved = crate::keymap::resolve_help_selection(&rows, selected);
    let show_scope = !query.trim().is_empty();

    if !rows.iter().any(crate::keymap::HelpRow::is_selectable) {
        let empty_msg = Line::from(Span::styled(
            format!("  {}", ctx.t("help.no_match")),
            Style::default().fg(ctx.theme.fg_muted),
        ));
        frame.render_widget(
            Paragraph::new(empty_msg).style(Style::default().bg(ctx.theme.bg_elevated)),
            list_rect,
        );
    } else {
        let vis_h = list_h as usize;
        let scroll_y = if resolved >= vis_h {
            (resolved + 1).saturating_sub(vis_h)
        } else {
            0
        };
        let key_col_w = 22usize;
        let mut lines: Vec<Line<'_>> = Vec::with_capacity(vis_h);

        for (idx, row) in rows.iter().enumerate().skip(scroll_y).take(vis_h) {
            match row {
                crate::keymap::HelpRow::Section(title) => {
                    lines.push(Line::from(Span::styled(
                        truncate_display_width(&format!(" {title}"), inner.width as usize),
                        Style::default()
                            .fg(ctx.theme.fg_accent_link)
                            .bg(ctx.theme.bg_elevated)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                crate::keymap::HelpRow::Entry {
                    keys,
                    desc,
                    scope_tag,
                    run,
                } => {
                    let is_sel = idx == resolved;
                    let row_bg = if is_sel {
                        ctx.theme.bg_selection_focused
                    } else {
                        ctx.theme.bg_elevated
                    };
                    let key_bg = if is_sel {
                        ctx.theme.bg_selection_focused
                    } else {
                        ctx.theme.bg_panel
                    };
                    let key_fg = if is_sel {
                        ctx.theme.fg_accent_link
                    } else {
                        ctx.theme.fg_bright
                    };
                    let desc_fg = if is_sel {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    };

                    let key_badge = format!(" {keys} ");
                    let pad_after = key_col_w.saturating_sub(2 + key_badge.width()).max(2);
                    let right_scope = if show_scope {
                        format!(" [{scope_tag}]")
                    } else {
                        String::new()
                    };
                    let enter_tag = if is_sel && run.is_some() {
                        " ↵ "
                    } else {
                        "  "
                    };
                    let left_fixed = 2 + key_badge.width() + pad_after;
                    let right_w = right_scope.width() + enter_tag.width();
                    let desc_budget = (inner.width as usize).saturating_sub(left_fixed + right_w);
                    let desc_disp = truncate_display_width(desc, desc_budget);
                    let mid_gap = (inner.width as usize)
                        .saturating_sub(left_fixed + desc_disp.width() + right_w);

                    let mut row_spans = vec![
                        Span::styled("  ", Style::default().bg(row_bg)),
                        Span::styled(
                            key_badge,
                            Style::default()
                                .fg(key_fg)
                                .bg(key_bg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ".repeat(pad_after), Style::default().bg(row_bg)),
                        Span::styled(desc_disp, Style::default().fg(desc_fg).bg(row_bg)),
                    ];
                    if mid_gap > 0 {
                        row_spans.push(Span::styled(
                            " ".repeat(mid_gap),
                            Style::default().bg(row_bg),
                        ));
                    }
                    if !right_scope.is_empty() {
                        row_spans.push(Span::styled(
                            right_scope,
                            Style::default().fg(ctx.theme.fg_muted).bg(row_bg),
                        ));
                    }
                    row_spans.push(Span::styled(
                        enter_tag,
                        Style::default()
                            .fg(ctx.theme.fg_accent_link)
                            .bg(row_bg)
                            .add_modifier(Modifier::BOLD),
                    ));
                    lines.push(Line::from(row_spans));
                }
            }
        }
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_elevated)),
            list_rect,
        );
    }

    if has_git_row && let Some((summary, is_err)) = git_summary {
        let style = if is_err {
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        let git_line = Line::from(Span::styled(
            truncate_display_width(&format!(" {summary}"), inner.width as usize),
            style,
        ));
        frame.render_widget(
            Paragraph::new(git_line).style(Style::default().bg(ctx.theme.bg_panel)),
            git_rect,
        );
    }

    let foot_line = Line::from(Span::styled(
        truncate_display_width(
            &format!(" {}", ctx.t("help.footer_hint")),
            inner.width as usize,
        ),
        Style::default()
            .fg(ctx.theme.fg_muted)
            .bg(ctx.theme.bg_panel),
    ));
    frame.render_widget(
        Paragraph::new(foot_line).style(Style::default().bg(ctx.theme.bg_panel)),
        foot_rect,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_filter_editor_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    field: FilterField,
    query: &str,
    selected: usize,
    selection_touched: bool,
) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(ctx.theme.fg_accent_link)),
            Span::styled(
                format!("{query}▏"),
                Style::default().fg(ctx.theme.fg_bright),
            ),
        ]),
        Line::from(Span::styled(
            format!(
                " {}",
                ctx.i18n
                    .map_or(field.hint(), |i| i.filter_field_hint(field))
            ),
            Style::default().fg(ctx.theme.fg_muted),
        )),
        Line::from(Span::styled(
            ctx.t("filter.editor_hint").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )),
    ];
    let has_favorites = !state
        .log
        .favorite_branch_names(state.repo_snapshot.as_ref())
        .is_empty();
    let history = state.log.filter.history_for(field);
    let suggestions = crate::log_view::filter_suggestions_with_context(
        field,
        state.repo_snapshot.as_deref(),
        query,
        has_favorites,
        history,
    );
    let room = inner.height.saturating_sub(3) as usize;
    let sel = selected.min(suggestions.len().saturating_sub(1));
    let start = sel.saturating_sub(room.saturating_sub(1));
    for (idx, s) in suggestions.iter().enumerate().skip(start).take(room) {
        let bg = if idx == sel {
            if selection_touched || !query.is_empty() {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_hover
            }
        } else {
            ctx.theme.bg_elevated
        };
        lines.push(
            Line::from(Span::styled(
                format!(
                    "   {}",
                    truncate_to_width(s, inner.width.saturating_sub(4) as usize)
                ),
                Style::default().fg(ctx.theme.fg_primary),
            ))
            .style(Style::default().bg(bg)),
        );
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_goto_revision_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    query: &str,
) {
    let resolved = state
        .repo_snapshot
        .as_ref()
        .and_then(|s| s.resolve_revision(query).map(|oid| (s, oid)));
    let preview = match resolved {
        Some((s, oid)) => {
            let subject = s
                .commits
                .iter()
                .find(|c| c.oid == oid)
                .map(|c| c.subject.as_str())
                .unwrap_or("");
            Line::from(vec![
                Span::styled(
                    format!(" → {} ", oid.short()),
                    Style::default().fg(ctx.theme.fg_accent_link),
                ),
                Span::styled(
                    truncate_to_width(subject, inner.width.saturating_sub(12) as usize),
                    Style::default().fg(ctx.theme.fg_primary),
                ),
            ])
        }
        None if query.is_empty() => Line::from(""),
        None => Line::from(Span::styled(
            ctx.t("popup.goto_no_match").to_string(),
            Style::default().fg(ctx.theme.status_conflicted),
        )),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(ctx.theme.fg_accent_link)),
            Span::styled(
                format!("{query}▏"),
                Style::default().fg(ctx.theme.fg_bright),
            ),
        ]),
        preview,
        Line::from(Span::styled(
            ctx.t("popup.goto_hint").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_choose_adjacent_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    is_parent: bool,
    selected: usize,
) {
    let mut lines = Vec::new();
    let candidates: Vec<jbgit_core::Oid> = if is_parent {
        let vis = state
            .log
            .graph
            .parent_candidates(state.repo_snapshot.as_ref());
        if !vis.is_empty() {
            vis.into_iter().map(|(_, oid)| oid).collect()
        } else {
            state
                .log_commit_at(state.log.graph.selected_idx)
                .map(|c| c.parents.clone())
                .unwrap_or_default()
        }
    } else {
        state
            .log
            .graph
            .child_candidates(state.repo_snapshot.as_ref())
            .into_iter()
            .map(|(_, oid)| oid)
            .collect()
    };

    if let Some(snap) = &state.repo_snapshot
        && !candidates.is_empty()
    {
        let now = chrono::Local::now().timestamp();
        let sel = selected.min(candidates.len().saturating_sub(1));
        for (idx, oid) in candidates.iter().enumerate() {
            let commit = snap.commits.iter().find(|c| c.oid == *oid);
            let bg = if idx == sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let prefix = format!(" {}. {} ", idx + 1, oid.short());
            if let Some(c) = commit {
                let date = ctx.format_relative_time(c.timestamp, now);
                let meta = format!("  {} · {}", c.author_name, date);
                let subj_budget =
                    (inner.width as usize).saturating_sub(prefix.width() + meta.width() + 2);
                let quoted_subj = format!("\"{}\"", truncate_to_width(&c.subject, subj_budget));
                lines.push(
                    Line::from(vec![
                        Span::styled(prefix, Style::default().fg(ctx.theme.fg_accent_link)),
                        Span::styled(quoted_subj, Style::default().fg(ctx.theme.fg_bright)),
                        Span::styled(meta, Style::default().fg(ctx.theme.fg_muted)),
                    ])
                    .style(Style::default().bg(bg)),
                );
            } else {
                let fallback = ctx.t("popup.choose_parent_outside");
                let max_subj = (inner.width as usize).saturating_sub(prefix.width());
                lines.push(
                    Line::from(vec![
                        Span::styled(prefix, Style::default().fg(ctx.theme.fg_accent_link)),
                        Span::styled(
                            truncate_to_width(fallback, max_subj),
                            Style::default().fg(ctx.theme.fg_muted),
                        ),
                    ])
                    .style(Style::default().bg(bg)),
                );
            }
        }
    }
    if lines.is_empty() {
        let key = if is_parent {
            "popup.choose_parent_none"
        } else {
            "popup.choose_child_none"
        };
        lines.push(Line::from(Span::styled(
            ctx.t(key).to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_commit_message_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    oid: jbgit_core::Oid,
    subject: &str,
    message: &str,
    scroll: usize,
    z: u8,
) {
    let short = oid.short();
    let title = ctx.t_fmt(
        "popup.commit_message_title",
        &[
            ("short", &short),
            ("subject", &truncate_to_width(subject, 48)),
        ],
    );
    let max_w = screen.width.saturating_sub(6).clamp(40, 100);
    let max_h = screen.height.saturating_sub(4).clamp(10, 36);
    let inner = render_centered_popup_box(frame, screen, ctx, max_w, max_h, title, z);
    if inner.width < 8 || inner.height < 3 {
        return;
    }

    let body_h = inner.height.saturating_sub(1) as usize;
    let footer_rect = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    let width = inner.width.saturating_sub(2).max(8) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<(usize, u16, u16, jbgit_core::Oid)> = Vec::new();
    let snap = state.repo_snapshot.as_deref();

    for (idx, raw) in message.lines().enumerate() {
        let kind = jbgit_core::classify_message_line(idx, raw);
        let base = match kind {
            jbgit_core::MessageLineKind::Subject => Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
            jbgit_core::MessageLineKind::Trailer => {
                Style::default().fg(ctx.theme.ref_remote.pill_fg)
            }
            jbgit_core::MessageLineKind::Body => Style::default().fg(ctx.theme.fg_primary),
        };
        if raw.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        for chunk in crate::views::wrap_text(raw, width) {
            let line_idx = lines.len();
            let (spans, line_hits) =
                crate::views::log_details::commit_details::render_body_chunk_with_links(
                    &chunk, width, base, snap, ctx.theme,
                );
            for (hx, hw, target_oid) in line_hits {
                hits.push((line_idx, hx, hw, target_oid));
            }
            lines.push(Line::from(spans));
        }
    }

    let max_scroll = lines.len().saturating_sub(body_h);
    ctx.record_scroll_bound(
        crate::app::ScrollPane::CommitMessageModal,
        body_h,
        max_scroll,
    );
    let off = scroll.min(max_scroll);

    for (vis_y, idx) in (off..lines.len()).take(body_h).enumerate() {
        let rect = Rect::new(inner.x, inner.y + vis_y as u16, inner.width, 1);
        frame.render_widget(Paragraph::new(lines[idx].clone()), rect);
        for &(li, hx, hw, target_oid) in &hits {
            if li == idx && hx < inner.width {
                let w = hw.min(inner.width - hx);
                ctx.hit_registry.register(
                    Rect::new(inner.x + hx, rect.y, w, 1),
                    z + 2,
                    HitTarget::DetailsCommitHash { oid: target_oid },
                );
            }
        }
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(
                ctx.t("popup.commit_message_footer"),
                footer_rect.width as usize,
            ),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        footer_rect,
    );
}

fn render_commit_history_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    query: &str,
    selected: usize,
) {
    let candidates = state
        .stage_workbench
        .commit_history_candidates(state.repo_snapshot.as_deref());
    let q_low = query.to_ascii_lowercase();
    let filtered: Vec<(usize, &String)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, m)| q_low.is_empty() || m.to_ascii_lowercase().contains(&q_low))
        .collect();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            ctx.t("popup.commit_history_filter").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(
            format!("{query}▏"),
            Style::default().fg(ctx.theme.fg_bright),
        ),
    ])];
    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            ctx.t("popup.commit_history_empty").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    } else {
        let sel = selected.min(filtered.len() - 1);
        let room = inner.height.saturating_sub(2) as usize;
        let start = sel.saturating_sub(room.saturating_sub(1));
        for (vis, (_, msg)) in filtered.iter().enumerate().skip(start).take(room) {
            let is_sel = vis == sel;
            let bg = if is_sel {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_elevated
            };
            let first_line = msg.lines().next().unwrap_or("");
            lines.push(
                Line::from(Span::styled(
                    format!(
                        " {}",
                        truncate_to_width(first_line, inner.width.saturating_sub(2) as usize)
                    ),
                    Style::default().fg(if is_sel {
                        ctx.theme.fg_bright
                    } else {
                        ctx.theme.fg_primary
                    }),
                ))
                .style(Style::default().bg(bg)),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_three_way_inspector_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
    range_idx: Option<usize>,
) {
    let mut lines = Vec::new();
    if let Some(diff) = state.stage_workbench.current_diff()
        && let Some(range) = state.stage_workbench.three_way_range_for_popup(range_idx)
    {
        let max_w = inner.width.saturating_sub(4) as usize;
        let local_rows = diff.tracker.local_doc.slice_display(range.local_range);
        let staged_rows = diff.tracker.staged_doc.slice_display(range.staged_range);
        let vcs_rows = diff.tracker.vcs_doc.slice_display(range.vcs_range);

        let compute_ranges = |primary: &[String], other_a: &[String], other_b: &[String]| {
            primary
                .iter()
                .enumerate()
                .map(|(idx, line)| {
                    let mut ranges = Vec::new();
                    if let Some(a) = other_a.get(idx) {
                        let (r, _) = word_diff(line, a);
                        ranges.extend(r);
                    }
                    if let Some(b) = other_b.get(idx) {
                        let (r, _) = word_diff(line, b);
                        ranges.extend(r);
                    }
                    ranges
                })
                .collect::<Vec<_>>()
        };

        let local_word_ranges = compute_ranges(&local_rows, &staged_rows, &vcs_rows);
        let staged_word_ranges = compute_ranges(&staged_rows, &vcs_rows, &local_rows);
        let vcs_word_ranges = compute_ranges(&vcs_rows, &staged_rows, &local_rows);

        let mut push_section =
            |title: &str,
             color: ratatui::style::Color,
             word_bg: ratatui::style::Color,
             rows: &[String],
             word_ranges: &[Vec<(usize, usize)>]| {
                lines.push(Line::from(Span::styled(
                    format!(" ── {title} ──"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )));
                if rows.is_empty() {
                    lines.push(Line::from(Span::styled(
                        ctx.t("popup.three_way_empty").to_string(),
                        Style::default().fg(ctx.theme.fg_muted),
                    )));
                } else {
                    let base = Style::default().fg(ctx.theme.fg_primary);
                    for (idx, r) in rows.iter().enumerate() {
                        let wr = word_ranges.get(idx).map(Vec::as_slice).unwrap_or(&[]);
                        let mut spans = vec![Span::raw("   ")];
                        spans.extend(render_diff_line_spans_with_tab(
                            r,
                            &[],
                            wr,
                            max_w,
                            ctx.diff_tab_size(),
                            base,
                            word_bg,
                            ctx.theme.true_color,
                        ));
                        lines.push(Line::from(spans));
                    }
                }
            };
        push_section(
            ctx.t("popup.three_way_worktree"),
            ctx.theme.status_added,
            ctx.theme.diff_add_word_bg,
            &local_rows,
            &local_word_ranges,
        );
        push_section(
            ctx.t("popup.three_way_staged"),
            ctx.theme.status_modified,
            ctx.theme.diff_mod_bg,
            &staged_rows,
            &staged_word_ranges,
        );
        push_section(
            ctx.t("popup.three_way_head"),
            ctx.theme.fg_muted,
            ctx.theme.diff_del_word_bg,
            &vcs_rows,
            &vcs_word_ranges,
        );
    } else {
        lines.push(Line::from(Span::styled(
            ctx.t("popup.three_way_hint").to_string(),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_hook_error_popup(
    frame: &mut Frame<'_>,
    inner: Rect,
    ctx: &RenderContext<'_>,
    output: &str,
) {
    let msg = if output.is_empty() {
        ctx.t("popup.hook_error_fallback")
    } else {
        output
    };
    let mut lines: Vec<Line<'_>> = msg
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                format!(" {l}"),
                Style::default().fg(ctx.theme.status_conflicted),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        ctx.t("popup.hook_error_preserved").to_string(),
        Style::default().fg(ctx.theme.fg_muted),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
