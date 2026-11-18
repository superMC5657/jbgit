use jgit_core::{
    ChangeNodeId, PaneId, SplitAxis, SplitterId, StageNodeKind, StageTreeRowKind, TreeId,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{CommitOptionKind, HitTarget, StageToolbarButton};
use crate::views::commit_diff::render_stage_diff_workbench;
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, render_splitter_divider,
    row_background, truncate_to_width,
};

pub fn render_commit_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let diff_collapsed = state.stage_workbench.diff_collapsed;
    let (left_area, right_area, main_divider, main_container, main_collapsed) = if diff_collapsed {
        (area, Rect::ZERO, Rect::ZERO, area, true)
    } else {
        let main_split = state.splitters.split(
            area,
            SplitterId::CommitMainHorizontal,
            SplitAxis::Horizontal,
        );
        splitter_containers.push((
            SplitterId::CommitMainHorizontal,
            SplitAxis::Horizontal,
            main_split.container,
        ));
        (
            main_split.first,
            main_split.second,
            main_split.divider,
            main_split.container,
            main_split.collapsed,
        )
    };
    let _ = main_container;

    let left_split = state.splitters.split(
        left_area,
        SplitterId::CommitLeftVertical,
        SplitAxis::Vertical,
    );
    splitter_containers.push((
        SplitterId::CommitLeftVertical,
        SplitAxis::Vertical,
        left_split.container,
    ));

    // Left-top: GitStageTree
    let total_changes = state.stage_status.as_ref().map_or(0, |s| s.total_changes());
    let total_s = total_changes.to_string();
    let changes_sub = state.i18n.t_fmt("view.commit.files_count", &[("count", &total_s)]);
    let changes_body = render_pane_frame(
        frame,
        left_split.first,
        PaneId::CommitChangesTree,
        &changes_sub,
        ctx,
    );
    render_stage_tree_pane(frame, changes_body, state, ctx);

    // Left-bottom: CommitPanel
    if !left_split.collapsed {
        render_splitter_divider(
            frame,
            left_split.divider,
            SplitterId::CommitLeftVertical,
            SplitAxis::Vertical,
            ctx,
        );

        let staged_count = state
            .stage_status
            .as_ref()
            .map_or(0, |s| s.staged.len());
        let staged_s = staged_count.to_string();
        let commit_sub = if state.stage_workbench.amend {
            state.i18n.t("view.commit.amend_head").to_string()
        } else {
            state.i18n.t_fmt("view.commit.staged_count", &[("count", &staged_s)])
        };
        let msg_body = render_pane_frame(
            frame,
            left_split.second,
            PaneId::CommitMessageEditor,
            &commit_sub,
            ctx,
        );
        render_commit_panel(frame, msg_body, state, ctx);
    }

    // Right: Interactive StageDiffWorkbench
    if !main_collapsed {
        render_splitter_divider(
            frame,
            main_divider,
            SplitterId::CommitMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );
        render_stage_diff_workbench(frame, right_area, state, ctx);
    }
}

fn render_stage_tree_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let toolbar_rect = Rect::new(area.x, area.y, area.width, 1);
    render_stage_toolbar(frame, toolbar_rect, state, ctx);

    let list_area = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    if list_area.height == 0 {
        return;
    }

    let Some(status) = &state.stage_status else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                state.i18n.t("view.commit.loading_stage"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            list_area,
        );
        return;
    };

    let rows = &state.stage_workbench.rows;
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                state.i18n.t("view.commit.working_clean"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            list_area,
        );
        return;
    }

    let visible_h = list_area.height as usize;
    let (selected, offset) = list_viewport(rows.len(), state.commit_stage_selected_idx, visible_h);
    let focused = ctx.focused_pane == PaneId::CommitChangesTree;
    let glyphs = ctx.glyph_mode.symbols();

    for (vis_idx, row_idx) in (offset..rows.len()).take(visible_h).enumerate() {
        let row = &rows[row_idx];
        let row_rect = Rect::new(
            list_area.x,
            list_area.y.saturating_add(vis_idx as u16),
            list_area.width,
            1,
        );
        let is_sel = row_idx == selected || state.stage_workbench.multi_selected.contains(&row.key);
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::ChangesTreeNode { node_id, .. }) if node_id.0 as usize == row_idx
        );
        let bg = row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);
        let indent = "  ".repeat(row.depth as usize);

        let mut left: Vec<Span<'static>> = vec![Span::raw(format!(" {indent}"))];
        let mut right: Vec<Span<'static>> = Vec::new();
        let mut hover_action: Option<(&'static str, ratatui::style::Color)> = None;
        let mut resolve_pill_w = 0u16;

        match &row.kind {
            StageTreeRowKind::Group { kind, file_count } => {
                let arrow = if row.expanded {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                let title_fg = match kind {
                    StageNodeKind::Conflicted => ctx.theme.status_conflicted,
                    StageNodeKind::Staged => ctx.theme.status_added,
                    StageNodeKind::Unstaged => ctx.theme.status_modified,
                    StageNodeKind::Untracked => ctx.theme.status_untracked,
                    StageNodeKind::Ignored | StageNodeKind::AmendCommit => ctx.theme.fg_muted,
                };
                left.push(Span::styled(
                    format!("{arrow} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    format!("{} ({file_count})", row.label),
                    Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
                ));
                if *kind == StageNodeKind::Conflicted {
                    let pill = state.i18n.t("view.commit.resolve_pill").to_string();
                    resolve_pill_w = pill.width() as u16;
                    right.push(Span::styled(
                        pill,
                        Style::default()
                            .fg(ctx.theme.fg_bright)
                            .bg(ctx.theme.status_conflicted)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if is_hovered {
                    if kind.can_stage() {
                        hover_action = Some((" [+] ", ctx.theme.status_added));
                    } else if kind.can_unstage() {
                        hover_action = Some((" [-] ", ctx.theme.status_untracked));
                    }
                }
            }
            StageTreeRowKind::Dir {
                group_kind,
                file_count,
                ..
            } => {
                let arrow = if row.expanded {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                left.push(Span::styled(
                    format!("{arrow} {} ", glyphs.files.folder),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    row.label.clone(),
                    Style::default().fg(ctx.theme.fg_primary),
                ));
                right.push(Span::styled(
                    format!("{file_count} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                if is_hovered {
                    if group_kind.can_stage() {
                        hover_action = Some((" [+] ", ctx.theme.status_added));
                    } else if group_kind.can_unstage() {
                        hover_action = Some((" [-] ", ctx.theme.status_untracked));
                    }
                }
            }
            StageTreeRowKind::File {
                group_kind,
                path,
                old_path,
                status_kind,
                conflict_label,
            } => {
                let fg_kind = ctx.theme.status_color(*status_kind);
                left.push(Span::styled(
                    format!("{} ", status_kind.badge_char()),
                    Style::default().fg(fg_kind).add_modifier(Modifier::BOLD),
                ));
                left.push(Span::styled(
                    format!("{} ", glyphs.file_icon(path)),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    row.label.clone(),
                    Style::default().fg(if is_sel {
                        ctx.theme.fg_bright
                    } else {
                        fg_kind
                    }),
                ));
                if let Some(old) = old_path {
                    left.push(Span::styled(
                        format!(" ← {}", old.rsplit('/').next().unwrap_or(old)),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
                if let Some(clabel) = conflict_label {
                    right.push(Span::styled(
                        format!("{clabel} "),
                        Style::default().fg(ctx.theme.status_conflicted),
                    ));
                } else if is_hovered {
                    if group_kind.can_stage() {
                        hover_action = Some((" [+] ", ctx.theme.status_added));
                    } else if group_kind.can_unstage() {
                        hover_action = Some((" [-] ", ctx.theme.status_untracked));
                    }
                }
            }
            StageTreeRowKind::AmendFile {
                path,
                old_path,
                status_kind,
            } => {
                left.push(Span::styled(
                    format!("{} {} ", status_kind.badge_char(), glyphs.file_icon(path)),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    row.label.clone(),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                if let Some(old) = old_path {
                    left.push(Span::styled(
                        format!(" ← {}", old.rsplit('/').next().unwrap_or(old)),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
            }
        }

        let mut hover_btn_w = 0u16;
        if let Some((sym, col)) = hover_action {
            hover_btn_w = sym.width() as u16;
            right.push(Span::styled(
                sym,
                Style::default()
                    .fg(col)
                    .bg(ctx.theme.bg_elevated)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let spans = fit_left_right_spans(left, right, row_rect.width as usize, 0);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );

        ctx.hit_registry.register(
            row_rect,
            2,
            HitTarget::ChangesTreeNode {
                tree_id: TreeId(0),
                node_id: ChangeNodeId(row_idx as u32),
                on_hover_action_icon: false,
            },
        );
        if resolve_pill_w > 0 && row_rect.width >= resolve_pill_w {
            let rx = row_rect.right().saturating_sub(resolve_pill_w);
            ctx.hit_registry.register(
                Rect::new(rx, row_rect.y, resolve_pill_w, 1),
                4,
                HitTarget::StageResolveConflicts,
            );
        }
        if hover_btn_w > 0 && row_rect.width >= hover_btn_w {
            let hx = row_rect.right().saturating_sub(hover_btn_w);
            ctx.hit_registry.register(
                Rect::new(hx, row_rect.y, hover_btn_w, 1),
                4,
                HitTarget::ChangesTreeNode {
                    tree_id: TreeId(0),
                    node_id: ChangeNodeId(row_idx as u32),
                    on_hover_action_icon: true,
                },
            );
        }
    }
    let _ = status;
}

fn render_stage_toolbar(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let sw = &state.stage_workbench;
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );

    let buttons = [
        (" ↻ ", false, StageToolbarButton::Refresh),
        (" + Tracked ", false, StageToolbarButton::StageAllTracked),
        (" Stash ", false, StageToolbarButton::Stash),
        (
            " ⊞ Dir ",
            sw.tree_opts.group_by_dir,
            StageToolbarButton::GroupByDir,
        ),
        (
            " ◉ Diff ",
            !sw.diff_collapsed,
            StageToolbarButton::ToggleDiff,
        ),
        (" ⚙ ", sw.tree_opts.show_ignored, StageToolbarButton::Options),
    ];

    let mut x = area.x;
    for (label, active, btn) in buttons {
        let w = label.width() as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let style = if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_unfocused)
        } else {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
        ctx.hit_registry
            .register(rect, 3, HitTarget::StageToolbar(btn));
        x += w + 1;
    }
}

fn render_commit_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let sw = &state.stage_workbench;
    let mut y = area.y;
    let bottom = area.bottom();

    // Row 0: Commit options checkboxes + history + author toggle
    if y < bottom {
        let opt_rect = Rect::new(area.x, y, area.width, 1);
        render_commit_options_row(frame, opt_rect, sw, ctx);
        y += 1;
    }

    // Optional Author override row
    if sw.show_author_input && y < bottom {
        let auth_rect = Rect::new(area.x, y, area.width, 1);
        let prefix = format!(" {}: ", state.i18n.t("view.commit.author"));
        let cursor = if sw.editing_author { "▏" } else { "" };
        let val = if sw.author_input.is_empty() && !sw.editing_author {
            state.i18n.t("view.commit.author_placeholder")
        } else {
            &sw.author_input
        };
        let line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(
                format!(
                    "{}{cursor}",
                    truncate_to_width(val, area.width.saturating_sub(11) as usize)
                ),
                Style::default().fg(if sw.editing_author {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(ctx.theme.bg_elevated)),
            auth_rect,
        );
        ctx.hit_registry.register(
            auth_rect,
            3,
            HitTarget::CommitOption(CommitOptionKind::AuthorInput),
        );
        y += 1;
    }

    // Reserve bottom rows for Action buttons (and optional Undo Commit / Progress banner)
    let reserve_banner = u16::from(sw.last_commit_banner.is_some() || sw.commit_in_progress);
    let reserve_btns = u16::from(y + 1 < bottom);
    let editor_h = bottom
        .saturating_sub(y)
        .saturating_sub(reserve_btns + reserve_banner);

    // Editor area
    if editor_h > 0 {
        let ed_rect = Rect::new(area.x, y, area.width, editor_h);
        render_commit_message_editor(frame, ed_rect, state, ctx);
        y += editor_h;
    }

    // Action buttons row
    if reserve_btns > 0 && y < bottom {
        let btn_rect = Rect::new(area.x, y, area.width, 1);
        render_commit_buttons_row(frame, btn_rect, state, ctx);
        y += 1;
    }

    // Bottom status/undo banner
    if reserve_banner > 0 && y < bottom {
        let banner_rect = Rect::new(area.x, y, area.width, 1);
        if sw.commit_in_progress {
            let spinner_s = state.spinner_char().to_string();
            let line = Line::from(Span::styled(
                state.i18n.t_fmt("view.commit.running_hooks", &[("spinner", &spinner_s)]),
                Style::default().fg(ctx.theme.border_focused),
            ));
            frame.render_widget(
                Paragraph::new(line).style(Style::default().bg(ctx.theme.bg_panel)),
                banner_rect,
            );
        } else if let Some(banner) = &sw.last_commit_banner {
            let left = vec![Span::styled(
                format!(" ✓ {}", banner.summary),
                Style::default().fg(ctx.theme.status_added),
            )];
            let undo_label = state.i18n.t("view.commit.undo_commit_btn").to_string();
            let right = if banner.can_undo {
                vec![Span::styled(
                    undo_label.clone(),
                    Style::default()
                        .fg(ctx.theme.fg_accent_link)
                        .bg(ctx.theme.bg_elevated)
                        .add_modifier(Modifier::BOLD),
                )]
            } else {
                Vec::new()
            };
            let spans = fit_left_right_spans(left, right, banner_rect.width as usize, 0);
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
                banner_rect,
            );
            if banner.can_undo && banner_rect.width >= undo_label.width() as u16 {
                let ux = banner_rect.right().saturating_sub(undo_label.width() as u16);
                ctx.hit_registry.register(
                    Rect::new(ux, banner_rect.y, undo_label.width() as u16, 1),
                    4,
                    HitTarget::UndoCommitButton,
                );
            }
        }
    }
}

fn render_commit_options_row(
    frame: &mut Frame<'_>,
    area: Rect,
    sw: &crate::stage_view::StageWorkbenchState,
    ctx: &mut RenderContext<'_>,
) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
    let chk = |on: bool| if on { "x" } else { " " };
    let items = [
        (
            format!(" [{}] {} ", chk(sw.amend), ctx.t("view.commit.amend_checkbox")),
            sw.amend,
            CommitOptionKind::Amend,
        ),
        (
            format!(" [{}] {} ", chk(sw.signoff), ctx.t("view.commit.sign_off")),
            sw.signoff,
            CommitOptionKind::Signoff,
        ),
        (
            format!(" [{}] {} ", chk(sw.run_hooks), ctx.t("view.commit.hooks")),
            sw.run_hooks,
            CommitOptionKind::RunHooks,
        ),
        (
            format!(" {} ", ctx.t("view.commit.author")),
            sw.show_author_input,
            CommitOptionKind::AuthorToggle,
        ),
        (" 🕘 ".to_string(), false, CommitOptionKind::History),
        (
            " ✎ $EDITOR ".to_string(),
            false,
            CommitOptionKind::ExternalEditor,
        ),
    ];

    let mut x = area.x;
    for (text, active, kind) in items {
        let w = text.width() as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let style = if active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_unfocused)
        } else {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(text, style)), rect);
        ctx.hit_registry
            .register(rect, 3, HitTarget::CommitOption(kind));
        x += w + 1;
    }
}

fn render_commit_message_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let sw = &state.stage_workbench;
    let focused = ctx.focused_pane == PaneId::CommitMessageEditor && !sw.editing_author;
    ctx.hit_registry
        .register(area, 2, HitTarget::CommitEditorArea);

    let bg = if sw.empty_message_error {
        ctx.theme.diff_del_bg
    } else if focused {
        ctx.theme.bg_elevated
    } else {
        ctx.theme.bg_canvas
    };
    frame.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);

    // Reserve 1 top row for subject ruler / blank-line hint when area.height >= 2
    let (ruler_h, text_area) = if area.height >= 2 {
        (
            1u16,
            Rect::new(area.x, area.y + 1, area.width, area.height - 1),
        )
    } else {
        (0u16, area)
    };

    if ruler_h > 0 {
        let subj_len = sw.editor.subject_char_count();
        let len_color = if subj_len > 72 {
            ctx.theme.status_conflicted
        } else if subj_len > 50 {
            ctx.theme.ref_head.icon
        } else {
            ctx.theme.fg_muted
        };
        let hint_text = if sw.empty_message_error {
            state.i18n.t("view.commit.msg_err_empty")
        } else if sw.editor.needs_blank_second_line_hint() {
            state.i18n.t("view.commit.msg_hint_blank")
        } else if sw.editor.is_empty() {
            state.i18n.t("view.commit.msg_hint_empty")
        } else {
            state.i18n.t("view.commit.msg_hint_body")
        };
        let left = vec![Span::styled(
            hint_text.to_string(),
            Style::default().fg(if sw.empty_message_error {
                ctx.theme.status_conflicted
            } else {
                ctx.theme.fg_muted
            }),
        )];
        let right = vec![Span::styled(
            format!("{subj_len}/50 "),
            Style::default().fg(len_color),
        )];
        let spans = fit_left_right_spans(left, right, area.width as usize, 0);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(ctx.theme.bg_panel)),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    if text_area.height == 0 || text_area.width <= 2 {
        return;
    }

    let inner_w = text_area.width.saturating_sub(2) as usize;
    let (wrapped, (cur_row, cur_col)) = sw.editor.visual_wrapped_lines(inner_w);
    let h = text_area.height as usize;
    let scroll = if cur_row >= sw.editor.scroll_row + h {
        cur_row + 1 - h
    } else if cur_row < sw.editor.scroll_row {
        cur_row
    } else {
        sw.editor.scroll_row.min(wrapped.len().saturating_sub(h))
    };

    for (vis_y, row_idx) in (scroll..wrapped.len()).take(h).enumerate() {
        let line_rect = Rect::new(text_area.x, text_area.y + vis_y as u16, text_area.width, 1);
        let wl = &wrapped[row_idx];
        let text_fg = if wl.logical_line == 0 {
            ctx.theme.fg_bright
        } else {
            ctx.theme.fg_primary
        };

        let mut spans = vec![Span::raw(" ")];
        if focused && row_idx == cur_row {
            let (before, after_char, rest) = split_at_visual_col(&wl.text, cur_col);
            spans.push(Span::styled(before, Style::default().fg(text_fg)));
            let cursor_ch = after_char.unwrap_or(" ");
            spans.push(Span::styled(
                cursor_ch,
                Style::default()
                    .fg(ctx.theme.bg_canvas)
                    .bg(ctx.theme.fg_bright)
                    .add_modifier(Modifier::BOLD),
            ));
            if !rest.is_empty() {
                spans.push(Span::styled(rest, Style::default().fg(text_fg)));
            }
        } else {
            spans.push(Span::styled(wl.text.clone(), Style::default().fg(text_fg)));
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            line_rect,
        );
    }
}

fn split_at_visual_col(text: &str, target_col: usize) -> (String, Option<&str>, String) {
    use unicode_width::UnicodeWidthChar;
    let mut w = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if w >= target_col {
            let next_b = byte_idx + ch.len_utf8();
            return (
                text[..byte_idx].to_string(),
                Some(&text[byte_idx..next_b]),
                text[next_b..].to_string(),
            );
        }
        w += ch.width().unwrap_or(0);
    }
    (text.to_string(), None, String::new())
}

fn render_commit_buttons_row(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
    let sw = &state.stage_workbench;
    let commit_label = if sw.amend {
        state.i18n.t("view.commit.amend_commit_btn")
    } else {
        state.i18n.t("view.commit.commit_btn")
    };
    let push_label = state.i18n.t("view.commit.commit_push_btn");

    let mut x = area.x.saturating_add(1);
    let cw = commit_label.width() as u16;
    if x + cw <= area.right() {
        let rect = Rect::new(x, area.y, cw, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                commit_label,
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.border_focused)
                    .add_modifier(Modifier::BOLD),
            )),
            rect,
        );
        ctx.hit_registry
            .register(rect, 4, HitTarget::CommitButton { and_push: false });
        x += cw + 1;
    }

    let pw = push_label.width() as u16;
    if x + pw <= area.right() {
        let rect = Rect::new(x, area.y, pw, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                push_label,
                Style::default()
                    .fg(ctx.theme.fg_primary)
                    .bg(ctx.theme.bg_elevated),
            )),
            rect,
        );
        ctx.hit_registry
            .register(rect, 4, HitTarget::CommitButton { and_push: true });
    }
}
