//! Built-in three-pane 3-way conflict merge resolver (`Ours` | `Result` | `Theirs`)
//! with optional syntax highlighting, intra-block word-diff, horizontal scrolling,
//! expandable `:1` Base ancestor panel, and lightweight inline multi-line Result editing.

use jbgit_core::{ConflictChoice, ConflictRowPlaceholder, ThreeWayBlockKind, word_diff};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{AppState, RenderContext};
use crate::hit_test::{ConflictMergeButtonKind, HitTarget};
use crate::syntax::{highlight_single_line, render_diff_line_spans_with_tab};
use crate::views::overlays::draw_shadow;
use crate::views::truncate_to_width;

fn skip_display_cols(text: &str, skip_cols: usize, tab_size: usize) -> usize {
    if skip_cols == 0 {
        return 0;
    }
    let tab_w = tab_size.clamp(1, 16);
    let mut col = 0usize;
    for (idx, ch) in text.char_indices() {
        if col >= skip_cols {
            return idx;
        }
        let adv = if ch == '\t' {
            tab_w - (col % tab_w)
        } else {
            ch.width().unwrap_or(0)
        };
        if col + adv > skip_cols {
            // Do not skip past a wide char that straddles the viewport left edge.
            return idx;
        }
        col += adv;
    }
    text.len()
}

fn is_conflict_placeholder(text: &str) -> bool {
    matches!(
        text,
        "<File will be deleted>" | "<Discarded (Neither)>" | "<Empty>"
    ) || text.starts_with("⚠ ")
        || text.starts_with("Non-conflicting (")
}

#[allow(clippy::too_many_arguments)]
fn build_pane_spans(
    prefix: &str,
    text: &str,
    path: &str,
    h_scroll: usize,
    word_ranges: &[(usize, usize)],
    col_w: u16,
    base_style: Style,
    word_bg: Color,
    syntax_on: bool,
    tab_size: usize,
    true_color: bool,
) -> Vec<Span<'static>> {
    let w = col_w as usize;
    if w == 0 {
        return Vec::new();
    }
    let prefix_w = prefix.width();
    let mut spans = Vec::new();
    if prefix_w > 0 && prefix_w <= w {
        spans.push(Span::styled(prefix.to_string(), base_style));
    }
    let content_w = w.saturating_sub(prefix_w);
    if content_w == 0 {
        return spans;
    }

    let is_placeholder = is_conflict_placeholder(text);
    let use_syntax = syntax_on && !is_placeholder && !text.is_empty();
    let raw_tokens = if use_syntax {
        highlight_single_line(path, text)
    } else {
        Vec::new()
    };

    let byte_skip = skip_display_cols(text, h_scroll, tab_size);
    let visible = &text[byte_skip..];
    let shifted_tokens: Vec<_> = raw_tokens
        .into_iter()
        .filter_map(|mut t| {
            if t.end <= byte_skip {
                return None;
            }
            t.start = t.start.saturating_sub(byte_skip);
            t.end = t.end.saturating_sub(byte_skip);
            (t.start < t.end).then_some(t)
        })
        .collect();
    let shifted_words: Vec<(usize, usize)> = word_ranges
        .iter()
        .filter_map(|&(s, e)| {
            if e <= byte_skip {
                return None;
            }
            let ns = s.saturating_sub(byte_skip);
            let ne = e.saturating_sub(byte_skip);
            (ns < ne).then_some((ns, ne))
        })
        .collect();

    let mut content_spans = render_diff_line_spans_with_tab(
        visible,
        &shifted_tokens,
        &shifted_words,
        content_w,
        tab_size,
        base_style,
        word_bg,
        true_color,
    );
    let rendered_w: usize = content_spans.iter().map(|s| s.content.width()).sum();
    if rendered_w < content_w {
        content_spans.push(Span::styled(" ".repeat(content_w - rendered_w), base_style));
    }
    spans.extend(content_spans);
    spans
}

pub fn render_conflict_merge_modal(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    z: u8,
) {
    if screen.width < 30 || screen.height < 10 {
        return;
    }
    let width = screen.width.saturating_sub(4).min(136);
    let height = screen.height.saturating_sub(2).min(34);
    let rect = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    draw_shadow(frame, screen, rect);
    crate::views::clear_modal_overlay(frame, rect);

    let Some(cm) = state.modal_stack.conflict_merge() else {
        return;
    };
    let i18n = &state.i18n;
    let title = match &cm.doc {
        Some(doc) => {
            let u = doc.unresolved_count().to_string();
            let t = doc.conflict_count.to_string();
            i18n.t_fmt(
                "conflict_merge.title",
                &[("path", &cm.path), ("unresolved", &u), ("total", &t)],
            )
        }
        None => i18n.t_fmt("conflict_merge.title_loading", &[("path", &cm.path)]),
    };
    let block = Block::default()
        .title(Span::styled(
            truncate_to_width(&title, rect.width.saturating_sub(4) as usize),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.status_modified))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::DialogBody);

    if inner.height < 4 {
        return;
    }

    let Some(doc) = &cm.doc else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                i18n.t("conflict_merge.loading"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            inner,
        );
        return;
    };

    let syntax_on = ctx.diff_syntax_enabled();
    let tab_size = ctx.diff_tab_size();
    let true_color = ctx.theme.true_color;

    let col_w = (inner.width.saturating_sub(2)) / 3;
    let right_w = inner.width.saturating_sub(col_w * 2 + 2);
    let header_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    let active_block = doc.block(cm.selected_block);
    let ours_title = active_block.map_or(i18n.t("conflict_merge.ours_default"), |b| {
        b.ours_label.as_str()
    });
    let theirs_title = active_block.map_or(i18n.t("conflict_merge.theirs_default"), |b| {
        b.theirs_label.as_str()
    });
    let ours_col_text = if doc.is_reversed {
        if ours_title == "replayed" || ours_title.is_empty() {
            i18n.t("conflict_merge.ours_col_rebase_plain").to_string()
        } else {
            i18n.t_fmt("conflict_merge.ours_col_rebase", &[("title", ours_title)])
        }
    } else {
        i18n.t_fmt("conflict_merge.ours_col", &[("title", ours_title)])
    };
    let theirs_col_text = if doc.is_reversed {
        if theirs_title == "onto" || theirs_title.is_empty() {
            i18n.t("conflict_merge.theirs_col_rebase_plain").to_string()
        } else {
            i18n.t_fmt(
                "conflict_merge.theirs_col_rebase",
                &[("title", theirs_title)],
            )
        }
    } else {
        i18n.t_fmt("conflict_merge.theirs_col", &[("title", theirs_title)])
    };
    let pad_col = |t: &str, w: u16| {
        let trunc = truncate_to_width(t, w as usize);
        let pad = (w as usize).saturating_sub(trunc.width());
        format!("{trunc}{}", " ".repeat(pad))
    };
    let hdr = Line::from(vec![
        Span::styled(
            pad_col(&ours_col_text, col_w),
            Style::default()
                .fg(ctx.theme.status_added)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad_col(i18n.t("conflict_merge.merged_col"), col_w),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("│", Style::default().fg(ctx.theme.border_splitter)),
        Span::styled(
            pad_col(&theirs_col_text, right_w),
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(hdr), header_rect);

    let stale_rows: u16 = if cm.externally_resolved {
        let warn_rect = Rect::new(inner.x, inner.y + 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(
                    i18n.t("conflict_merge.externally_resolved_banner"),
                    inner.width as usize,
                ),
                Style::default()
                    .fg(ctx.theme.status_conflicted)
                    .bg(ctx.theme.bg_panel)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(ctx.theme.bg_panel)),
            warn_rect,
        );
        1
    } else {
        0
    };

    // Reserve space for optional docked Base (:1) panel and/or Inline Result Editor.
    let footer_reserved: u16 = 2; // hint + buttons
    let max_dock_h = inner
        .height
        .saturating_sub(1 + stale_rows + footer_reserved + 3) as usize;
    let editor_panel_h: u16 = if let Some(ed) = &cm.inline_editor {
        (ed.lines.len().clamp(2, 6) + 1).min(max_dock_h) as u16
    } else {
        0
    };
    let base_panel_h: u16 = if cm.show_base && cm.inline_editor.is_none() {
        let content_lines = active_block.map_or(1, |b| b.base_lines.len().clamp(1, 5));
        (content_lines + 1).min(max_dock_h) as u16
    } else {
        0
    };
    let dock_h = editor_panel_h + base_panel_h;

    let body_start_y = inner.y + 1 + stale_rows;
    let body_h = inner
        .height
        .saturating_sub(1 + stale_rows + footer_reserved + dock_h) as usize;
    let rows = doc.three_pane_rows();
    let max_scroll = rows.len().saturating_sub(body_h.max(1));
    ctx.record_scroll_bound(
        crate::app::ScrollPane::ConflictMerge,
        body_h.max(1),
        max_scroll,
    );
    let scroll = cm.scroll.offset().min(max_scroll);
    for (vis, row) in rows.iter().skip(scroll).take(body_h).enumerate() {
        let y = body_start_y + vis as u16;
        let line_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_active = row.block_idx == Some(cm.selected_block);
        let is_non_conflict = row
            .block_kind
            .is_some_and(ThreeWayBlockKind::is_non_conflicting_change);
        let (ours_bg, mid_bg, theirs_bg) = match &row.choice {
            None => (
                ctx.theme.bg_canvas,
                ctx.theme.bg_canvas,
                ctx.theme.bg_canvas,
            ),
            Some(ConflictChoice::Unresolved) => {
                let hi = if is_active {
                    ctx.theme.bg_selection_focused
                } else if is_non_conflict {
                    ctx.theme.bg_panel
                } else {
                    ctx.theme.diff_mod_bg
                };
                (ctx.theme.diff_add_bg, hi, ctx.theme.diff_del_bg)
            }
            Some(_) => {
                let hi = if is_active {
                    ctx.theme.bg_selection_unfocused
                } else {
                    ctx.theme.diff_add_bg
                };
                (ctx.theme.diff_add_bg, hi, ctx.theme.diff_del_bg)
            }
        };
        let mid_fg = match &row.choice {
            Some(ConflictChoice::Unresolved) => {
                if is_non_conflict {
                    ctx.theme.fg_accent_link
                } else {
                    ctx.theme.status_conflicted
                }
            }
            Some(ConflictChoice::Neither) => ctx.theme.fg_muted,
            Some(_) => ctx.theme.fg_bright,
            None => ctx.theme.fg_primary,
        };
        let marker = if is_active { "▎" } else { " " };
        let ours_text: std::borrow::Cow<'_, str> = if row.ours_deleted_placeholder {
            std::borrow::Cow::Borrowed(i18n.t("conflict_merge.placeholder_deleted_side"))
        } else {
            std::borrow::Cow::Borrowed(&row.ours)
        };
        let theirs_text: std::borrow::Cow<'_, str> = if row.theirs_deleted_placeholder {
            std::borrow::Cow::Borrowed(i18n.t("conflict_merge.placeholder_deleted_side"))
        } else {
            std::borrow::Cow::Borrowed(&row.theirs)
        };
        let merged_text: std::borrow::Cow<'_, str> = match row.merged_placeholder {
            Some(ConflictRowPlaceholder::FileWillBeDeleted) => {
                std::borrow::Cow::Borrowed(i18n.t("conflict_merge.placeholder_file_deleted"))
            }
            Some(ConflictRowPlaceholder::DiscardedNeither) => {
                std::borrow::Cow::Borrowed(i18n.t("conflict_merge.placeholder_discarded"))
            }
            Some(ConflictRowPlaceholder::Empty) => {
                std::borrow::Cow::Borrowed(i18n.t("conflict_merge.placeholder_empty"))
            }
            Some(ConflictRowPlaceholder::UnresolvedLeftOnly { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(
                    i18n.t_fmt("conflict_merge.placeholder_block_left_only", &[("num", &n)]),
                )
            }
            Some(ConflictRowPlaceholder::UnresolvedRightOnly { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(i18n.t_fmt(
                    "conflict_merge.placeholder_block_right_only",
                    &[("num", &n)],
                ))
            }
            Some(ConflictRowPlaceholder::UnresolvedBothSame { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(
                    i18n.t_fmt("conflict_merge.placeholder_block_both_same", &[("num", &n)]),
                )
            }
            Some(ConflictRowPlaceholder::UnresolvedConflict { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(
                    i18n.t_fmt("conflict_merge.placeholder_block_conflict", &[("num", &n)]),
                )
            }
            Some(ConflictRowPlaceholder::UnresolvedDeletedByTheirs { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(i18n.t_fmt(
                    "conflict_merge.placeholder_block_deleted_by_theirs",
                    &[("num", &n)],
                ))
            }
            Some(ConflictRowPlaceholder::UnresolvedDeletedByOurs { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(i18n.t_fmt(
                    "conflict_merge.placeholder_block_deleted_by_ours",
                    &[("num", &n)],
                ))
            }
            Some(ConflictRowPlaceholder::UnresolvedBothDeleted { block_num }) => {
                let n = block_num.to_string();
                std::borrow::Cow::Owned(i18n.t_fmt(
                    "conflict_merge.placeholder_block_both_deleted",
                    &[("num", &n)],
                ))
            }
            None => std::borrow::Cow::Borrowed(&row.merged),
        };
        let (ours_words, theirs_words) = if row.block_kind == Some(ThreeWayBlockKind::Conflict)
            && !row.ours_deleted_placeholder
            && !row.theirs_deleted_placeholder
            && !row.ours.is_empty()
            && !row.theirs.is_empty()
            && row.ours != row.theirs
        {
            word_diff(&row.ours, &row.theirs)
        } else {
            (Vec::new(), Vec::new())
        };

        let mut line_spans = build_pane_spans(
            marker,
            &ours_text,
            &cm.path,
            cm.h_scroll,
            &ours_words,
            col_w,
            Style::default().fg(ctx.theme.fg_primary).bg(ours_bg),
            ctx.theme.diff_add_word_bg,
            syntax_on && !row.ours_deleted_placeholder,
            tab_size,
            true_color,
        );
        line_spans.push(Span::styled(
            "│",
            Style::default().fg(ctx.theme.border_splitter),
        ));
        line_spans.extend(build_pane_spans(
            " ",
            &merged_text,
            &cm.path,
            cm.h_scroll,
            &[],
            col_w,
            Style::default().fg(mid_fg).bg(mid_bg),
            ctx.theme.diff_add_word_bg,
            syntax_on && row.merged_placeholder.is_none(),
            tab_size,
            true_color,
        ));
        line_spans.push(Span::styled(
            "│",
            Style::default().fg(ctx.theme.border_splitter),
        ));
        line_spans.extend(build_pane_spans(
            " ",
            &theirs_text,
            &cm.path,
            cm.h_scroll,
            &theirs_words,
            right_w,
            Style::default().fg(ctx.theme.fg_primary).bg(theirs_bg),
            ctx.theme.diff_del_word_bg,
            syntax_on && !row.theirs_deleted_placeholder,
            tab_size,
            true_color,
        ));

        frame.render_widget(Paragraph::new(Line::from(line_spans)), line_rect);
        if let Some(bid) = row.block_idx {
            ctx.hit_registry
                .register(line_rect, z + 2, HitTarget::ConflictMergeBlock(bid));
        }
    }

    // Docked Base (:1) Ancestor panel.
    if base_panel_h > 0 {
        let base_y = body_start_y + body_h as u16;
        let blk_num = (cm.selected_block + 1).to_string();
        let hdr_text = i18n.t_fmt("conflict_merge.base_panel_title", &[("num", &blk_num)]);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&hdr_text, inner.width as usize),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_panel)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(ctx.theme.bg_panel)),
            Rect::new(inner.x, base_y, inner.width, 1),
        );
        let content_rows = (base_panel_h - 1) as usize;
        let base_lines: Vec<String> = active_block
            .map(|b| b.base_lines.clone())
            .unwrap_or_default();
        if base_lines.is_empty() {
            if content_rows > 0 {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        truncate_to_width(
                            i18n.t("conflict_merge.base_empty"),
                            inner.width as usize,
                        ),
                        Style::default()
                            .fg(ctx.theme.fg_muted)
                            .bg(ctx.theme.bg_canvas),
                    ))
                    .style(Style::default().bg(ctx.theme.bg_canvas)),
                    Rect::new(inner.x, base_y + 1, inner.width, 1),
                );
            }
        } else {
            for (idx, line) in base_lines.iter().take(content_rows).enumerate() {
                let spans = build_pane_spans(
                    " ",
                    line,
                    &cm.path,
                    cm.h_scroll,
                    &[],
                    inner.width,
                    Style::default()
                        .fg(ctx.theme.fg_primary)
                        .bg(ctx.theme.bg_canvas),
                    ctx.theme.diff_add_word_bg,
                    syntax_on,
                    tab_size,
                    true_color,
                );
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(inner.x, base_y + 1 + idx as u16, inner.width, 1),
                );
            }
        }
    }

    // Docked Inline Multi-Line Result Editor.
    if editor_panel_h > 0
        && let Some(ed) = &cm.inline_editor
    {
        let ed_y = body_start_y + body_h as u16;
        let blk_num = (ed.block_idx + 1).to_string();
        let hdr_text = i18n.t_fmt("conflict_merge.inline_edit_title", &[("num", &blk_num)]);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&hdr_text, inner.width as usize),
                Style::default()
                    .fg(ctx.theme.fg_bright)
                    .bg(ctx.theme.border_focused)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(ctx.theme.border_focused)),
            Rect::new(inner.x, ed_y, inner.width, 1),
        );
        let vis_rows = (editor_panel_h - 1) as usize;
        for row_off in 0..vis_rows {
            let line_idx = ed.v_scroll + row_off;
            let row_rect = Rect::new(inner.x, ed_y + 1 + row_off as u16, inner.width, 1);
            let Some(line) = ed.lines.get(line_idx) else {
                frame.render_widget(
                    Paragraph::new("").style(Style::default().bg(ctx.theme.bg_canvas)),
                    row_rect,
                );
                continue;
            };
            let is_cur_row = line_idx == ed.cursor_row;
            let row_bg = if is_cur_row {
                ctx.theme.bg_selection_unfocused
            } else {
                ctx.theme.bg_canvas
            };
            let prefix = format!("{:>3} ", line_idx + 1);
            let spans = build_pane_spans(
                &prefix,
                line,
                &cm.path,
                ed.h_scroll,
                &[],
                inner.width,
                Style::default().fg(ctx.theme.fg_bright).bg(row_bg),
                ctx.theme.diff_add_word_bg,
                syntax_on,
                tab_size,
                true_color,
            );
            frame.render_widget(Paragraph::new(Line::from(spans)), row_rect);
            if is_cur_row {
                let cur_col = ed.cursor_display_col().saturating_sub(ed.h_scroll);
                let cx = row_rect.x + prefix.width() as u16 + cur_col as u16;
                if cx < row_rect.right()
                    && let Some(cell) = frame.buffer_mut().cell_mut((cx, row_rect.y))
                {
                    cell.set_style(
                        Style::default()
                            .fg(ctx.theme.bg_canvas)
                            .bg(ctx.theme.fg_bright)
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
        }
    }

    let hint_y = inner.bottom().saturating_sub(2);
    let hint_text = if cm.inline_editor.is_some() {
        i18n.t("conflict_merge.hint_inline_edit")
    } else if doc.is_delete_modify() {
        i18n.t("conflict_merge.hint_delete_modify")
    } else {
        i18n.t("conflict_merge.hint")
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(hint_text, inner.width as usize),
            Style::default().fg(ctx.theme.fg_muted),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );

    let btn_y = inner.bottom().saturating_sub(1);
    let save_label = if doc.deletes_file() {
        i18n.t("conflict_merge.btn_delete_stage")
    } else if doc.unresolved_count() == 0 {
        i18n.t("conflict_merge.btn_save_stage")
    } else {
        i18n.t("conflict_merge.btn_save")
    };
    let auto_n = doc.non_conflicting_unresolved_count();
    let auto_label = i18n.t_fmt(
        "conflict_merge.btn_apply_auto",
        &[("count", &auto_n.to_string())],
    );
    let buttons: Vec<(String, ConflictMergeButtonKind, bool)> = if doc.is_delete_modify() {
        vec![
            (
                if doc.side_presence.ours_deleted() {
                    i18n.t("conflict_merge.btn_delete_file").to_string()
                } else {
                    i18n.t("conflict_merge.btn_keep_ours").to_string()
                },
                ConflictMergeButtonKind::AcceptOurs,
                false,
            ),
            (
                if doc.side_presence.theirs_deleted() {
                    i18n.t("conflict_merge.btn_delete_file").to_string()
                } else {
                    i18n.t("conflict_merge.btn_keep_theirs").to_string()
                },
                ConflictMergeButtonKind::AcceptTheirs,
                false,
            ),
            (
                i18n.t("conflict_merge.btn_base").to_string(),
                ConflictMergeButtonKind::ToggleBase,
                cm.show_base,
            ),
            (
                i18n.t("conflict_merge.btn_mergetool").to_string(),
                ConflictMergeButtonKind::Mergetool,
                false,
            ),
            (
                save_label.to_string(),
                ConflictMergeButtonKind::SaveAndStage,
                true,
            ),
            (
                i18n.t("conflict_merge.btn_close").to_string(),
                ConflictMergeButtonKind::Cancel,
                false,
            ),
        ]
    } else {
        let mut btns = Vec::new();
        if auto_n > 0 {
            btns.push((
                auto_label,
                ConflictMergeButtonKind::ApplyNonConflicting,
                false,
            ));
        }
        btns.push((
            i18n.t("conflict_merge.btn_ours").to_string(),
            ConflictMergeButtonKind::AcceptOurs,
            false,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_theirs").to_string(),
            ConflictMergeButtonKind::AcceptTheirs,
            false,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_both").to_string(),
            ConflictMergeButtonKind::AcceptBoth,
            false,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_neither").to_string(),
            ConflictMergeButtonKind::AcceptNeither,
            false,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_inline_edit").to_string(),
            ConflictMergeButtonKind::InlineEdit,
            cm.inline_editor.is_some(),
        ));
        btns.push((
            i18n.t("conflict_merge.btn_base").to_string(),
            ConflictMergeButtonKind::ToggleBase,
            cm.show_base,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_reset").to_string(),
            ConflictMergeButtonKind::ResetBlock,
            false,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_mergetool").to_string(),
            ConflictMergeButtonKind::Mergetool,
            false,
        ));
        btns.push((
            save_label.to_string(),
            ConflictMergeButtonKind::SaveAndStage,
            true,
        ));
        btns.push((
            i18n.t("conflict_merge.btn_close").to_string(),
            ConflictMergeButtonKind::Cancel,
            false,
        ));
        btns
    };
    let mut bx = inner.x + 1;
    for (label, kind, primary) in buttons {
        let w = label.width() as u16;
        if bx + w > inner.right() {
            break;
        }
        let r = Rect::new(bx, btn_y, w, 1);
        let style = if primary {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(label, style)), r);
        ctx.hit_registry
            .register(r, z + 2, HitTarget::ConflictMergeButton(kind));
        bx += w + 1;
    }
}
