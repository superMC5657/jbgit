use jbgit_core::{GroupOptions, ModalButton, RefInfo, group_refs_for_table, word_diff};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    AppState, ContextMenuEntryKind, ContextMenuItem, MenuParent, ModalLayer, PopupKind,
    RenderContext,
};
use crate::hit_test::HitTarget;
use crate::syntax::render_diff_line_spans_with_tab;
use crate::theme::RefBadgeKind;
use crate::views::palette_preview::render_palette_preview;
use crate::views::truncate_to_width;

/// Renders a hover tooltip when the pointer rests over a Detached HEAD pill or
/// a `+N` overflow pill and no modal layer is open.
pub fn render_hover_tooltip(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if !state.modal_stack.is_empty() || screen.width < 16 || screen.height < 5 {
        return;
    }
    let (Some(target), Some((hx, hy))) = (ctx.hover_target, state.hover_pos) else {
        return;
    };

    let (title, border_color, lines): (&str, _, Vec<Line<'_>>) = match target {
        HitTarget::CommitRefPill {
            is_detached_head: true,
            ..
        } => {
            let g = ctx.glyph_mode.symbols();
            (
                ctx.t("tooltip.detached_title"),
                ctx.theme.ref_detached.icon,
                vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", g.detached_head),
                            Style::default().fg(ctx.theme.ref_detached.icon),
                        ),
                        Span::styled(
                            ctx.t("tooltip.detached_line1").to_string(),
                            Style::default().fg(ctx.theme.ref_detached.pill_fg),
                        ),
                    ]),
                    Line::from(Span::styled(
                        ctx.t("tooltip.detached_line2").to_string(),
                        Style::default().fg(ctx.theme.fg_muted),
                    )),
                ],
            )
        }
        HitTarget::CommitRefOverflow {
            visible_row,
            hidden_refs,
        } => {
            let g = ctx.glyph_mode.symbols();
            let mut ref_lines = Vec::new();
            if let Some(snap) = &state.repo_snapshot
                && let Some(commit) = state.log_commit_at(*visible_row)
            {
                let is_head = snap.head_oid == Some(commit.oid);
                let head_marker = is_head.then(|| RefInfo::head_marker(commit.oid));
                let groups = group_refs_for_table(
                    snap.refs
                        .iter()
                        .filter(|r| r.target_oid == commit.oid)
                        .chain(head_marker.as_ref()),
                    GroupOptions::default(),
                );
                for grp in &groups {
                    let badge = RefBadgeKind::from_group(grp);
                    let style = ctx.theme.ref_style(badge);
                    let mut spans = vec![Span::raw(" ")];
                    for (icon, color) in ctx.theme.badge_icons(g, badge).into_iter().flatten() {
                        spans.push(Span::styled(icon, Style::default().fg(color)));
                    }
                    spans.push(Span::styled(
                        format!(" {}", grp.label),
                        Style::default().fg(style.pill_fg),
                    ));
                    ref_lines.push(Line::from(spans));
                }
            }
            if ref_lines.is_empty() {
                for name in hidden_refs {
                    ref_lines.push(Line::from(Span::styled(
                        format!(" {} {name}", g.local_branch),
                        Style::default().fg(ctx.theme.fg_primary),
                    )));
                }
            }
            ref_lines.push(Line::from(Span::styled(
                ctx.t("tooltip.overflow_hint").to_string(),
                Style::default().fg(ctx.theme.fg_muted),
            )));
            (
                ctx.t("tooltip.overflow_title"),
                ctx.theme.border_focused,
                ref_lines,
            )
        }
        _ => return,
    };

    let content_w = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(20)
        .max(title.width() + 2);
    let width = (content_w as u16 + 2).clamp(20, screen.width.saturating_sub(2).max(20));
    let height = (lines.len() as u16 + 2).min(screen.height.saturating_sub(1).max(3));

    let x = hx.min(screen.right().saturating_sub(width));
    let below_y = hy.saturating_add(1);
    let y = if below_y.saturating_add(height) <= screen.bottom() {
        below_y
    } else {
        hy.saturating_sub(height)
    };
    let rect = Rect::new(x, y, width, height);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Rows reserved for a context-menu item list (border excluded).
fn menu_visible_rows(rect: Rect) -> usize {
    rect.height.saturating_sub(2) as usize
}

/// Scroll offset keeping `selected` visible (deterministic for hit-testing).
pub fn menu_scroll(selected: usize, visible: usize, len: usize) -> usize {
    if visible == 0 || len <= visible || selected < visible {
        0
    } else {
        (selected + 1 - visible).min(len - visible)
    }
}

/// Screen row of item `idx` when `idx` is the selected item.
pub fn menu_item_row_y(_screen: Rect, rect: Rect, items: &[ContextMenuItem], idx: usize) -> u16 {
    let visible = menu_visible_rows(rect);
    let scroll = menu_scroll(idx, visible, items.len());
    rect.y + 1 + idx.saturating_sub(scroll) as u16
}

fn menu_has_icons(items: &[ContextMenuItem]) -> bool {
    items.iter().any(|i| i.icon.is_some())
}

/// Menu rectangle (shadow excluded) with viewport auto-flip: menus that would
/// overflow the right / bottom edge open to the left / upwards, and cascading
/// submenus open on the left of their parent when the right side is full.
pub fn context_menu_rect(
    screen: Rect,
    anchor: (u16, u16),
    parent: Option<MenuParent>,
    items: &[ContextMenuItem],
) -> Rect {
    let icon_w = if menu_has_icons(items) { 2 } else { 0 };
    let max_label = items
        .iter()
        .map(|i| {
            let suffix_w = match &i.kind {
                ContextMenuEntryKind::Submenu(_) => 3,
                ContextMenuEntryKind::Action(_) => i.shortcut.as_ref().map_or(0, |s| s.width() + 3),
                ContextMenuEntryKind::Separator => 0,
            };
            i.label.width() + suffix_w + 4 + icon_w
        })
        .max()
        .unwrap_or(20);

    // One extra column / row is reserved for the drop shadow.
    let max_w = screen.width.saturating_sub(1).max(1);
    let max_h = screen.height.saturating_sub(1).max(1);
    let width = (max_label as u16).clamp(22.min(max_w), max_w);
    let height = (items.len() as u16 + 2).clamp(3.min(max_h), max_h);
    let right_edge = screen.right();
    let bottom_edge = screen.bottom();
    let fits_right = |x: u16| x.saturating_add(width).saturating_add(1) <= right_edge;

    let (x, ay) = match parent {
        Some(p) => {
            let right_of = p.rect.right();
            let x = if fits_right(right_of) {
                right_of
            } else if p.rect.x >= screen.x.saturating_add(width) {
                p.rect.x - width
            } else {
                right_edge.saturating_sub(width + 1)
            };
            (x, p.row_y.saturating_sub(1))
        }
        None => {
            let x = if fits_right(anchor.0) {
                anchor.0
            } else {
                anchor.0.saturating_sub(width)
            };
            (x, anchor.1)
        }
    };
    let y = if ay.saturating_add(height).saturating_add(1) > bottom_edge {
        ay.saturating_add(1).saturating_sub(height)
    } else {
        ay
    };
    let x = x
        .max(screen.x)
        .min(right_edge.saturating_sub(width + 1).max(screen.x));
    let y = y
        .max(screen.y)
        .min(bottom_edge.saturating_sub(height + 1).max(screen.y));
    Rect::new(x, y, width, height)
}

pub fn render_modals(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    for layer in &state.modal_stack {
        let z = layer.z_level();
        // Register full-screen backdrop so lower-layer widgets cannot receive clicks
        // (cascading submenus share their root menu's backdrop so the parent
        // menu stays hoverable while a child is open).
        if !matches!(layer, ModalLayer::ContextMenu { depth, .. } if *depth > 0) {
            ctx.hit_registry
                .register(screen, z, HitTarget::ModalBackdrop { layer_z: z });
        }

        match layer {
            ModalLayer::FloatingPopup {
                kind,
                query,
                selected,
            } => render_floating_popup(frame, screen, state, ctx, *kind, query, *selected, z),
            ModalLayer::ContextMenu {
                anchor,
                items,
                selected,
                depth,
                parent,
            } => render_context_menu(
                frame, screen, ctx, *anchor, *parent, items, *selected, *depth, z,
            ),
            ModalLayer::Dialog(d) => crate::views::dialogs::render_dialog(frame, screen, ctx, d, z),
            ModalLayer::CriticalDialog {
                title,
                body,
                confirm_label,
                cancel_label,
                selected_button,
                ..
            } => render_critical_dialog(
                frame,
                screen,
                ctx,
                title,
                body,
                confirm_label,
                cancel_label,
                *selected_button,
                z,
            ),
        }
    }
}

pub fn render_which_key_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer_h: u16,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if !state.modal_stack.is_empty() || screen.width < 24 || screen.height < 6 {
        return;
    }
    let completions = crate::keymap::pending_completions(state);
    if completions.is_empty() {
        return;
    }

    let prefix = crate::keymap::pending_prefix_display(state);
    let title_str = format!(" {prefix} + … ");
    let visible: Vec<&(String, String)> = completions.iter().take(10).collect();

    let max_key_w = visible
        .iter()
        .map(|(k, _)| k.width())
        .max()
        .unwrap_or(2)
        .clamp(2, 12);
    let max_desc_w = visible
        .iter()
        .map(|(_, d)| d.width())
        .max()
        .unwrap_or(8)
        .clamp(8, 36);
    let cancel_hint = format!(" {}", ctx.t("help.which_key_cancel"));

    let content_w = (2 + max_key_w + 5 + max_desc_w + 1)
        .max(title_str.width() + 2)
        .max(cancel_hint.width() + 2);
    let w = ((content_w + 2) as u16)
        .clamp(30, 58)
        .min(screen.width.saturating_sub(2));
    let bottom_margin = footer_h.saturating_add(1);
    let h = ((visible.len() + 3) as u16).min(screen.height.saturating_sub(bottom_margin));
    if w < 16 || h < 5 {
        return;
    }

    let x = screen.x + screen.width.saturating_sub(w + 2);
    let y = screen.y + screen.height.saturating_sub(h + bottom_margin);
    let rect = Rect::new(x, y, w, h);

    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_focused))
        .style(Style::default().bg(ctx.theme.bg_elevated))
        .title(Span::styled(
            title_str,
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible.len() + 1);
    let desc_budget = (inner.width as usize).saturating_sub(2 + max_key_w + 5 + 1);
    for (k, d) in visible {
        let key_pad = " ".repeat(max_key_w.saturating_sub(k.width()));
        let trimmed_desc = truncate_display_width(d, desc_budget);
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(ctx.theme.bg_elevated)),
            Span::styled(
                k.clone(),
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_elevated)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{key_pad}  →  "),
                Style::default()
                    .fg(ctx.theme.fg_muted)
                    .bg(ctx.theme.bg_elevated),
            ),
            Span::styled(
                trimmed_desc,
                Style::default()
                    .fg(ctx.theme.fg_primary)
                    .bg(ctx.theme.bg_elevated),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        truncate_display_width(&cancel_hint, inner.width as usize),
        Style::default()
            .fg(ctx.theme.fg_muted)
            .bg(ctx.theme.bg_elevated),
    )));

    frame.render_widget(Paragraph::new(lines), inner);
}

fn truncate_display_width(text: &str, max_w: usize) -> String {
    if text.width() <= max_w {
        return text.to_string();
    }
    if max_w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    let limit = max_w.saturating_sub(1);
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > limit {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[allow(clippy::too_many_arguments)]
fn render_floating_popup(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    kind: PopupKind,
    query: &str,
    selected: usize,
    z: u8,
) {
    if kind == PopupKind::BranchesQuickView {
        crate::branches_popup::render_branches_popup(frame, screen, state, ctx, query, selected, z);
        return;
    }
    if kind == PopupKind::ConflictMerge {
        crate::views::conflict_merge::render_conflict_merge_modal(frame, screen, state, ctx, z);
        return;
    }
    let (max_w, max_h) = match kind {
        PopupKind::PalettePreview => (100, 38),
        PopupKind::ShortcutsHelp => (90, 38),
        PopupKind::GoToRevision => (64, 7),
        PopupKind::ChooseParent => (68, 12),
        PopupKind::CommitHistory => (76, 18),
        PopupKind::ThreeWayInspector => (84, 22),
        PopupKind::HookError => (84, 20),
        _ => (64, 18),
    };
    let width = screen.width.clamp(screen.width.min(24), max_w);
    let height = screen.height.clamp(screen.height.min(8), max_h);
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen
        .y
        .saturating_add(screen.height.saturating_sub(height) / 2);
    let rect = Rect::new(x, y, width, height);

    frame.render_widget(Clear, rect);

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
            let field_name = ctx.i18n.map_or(field.title(), |i| i.filter_field_title(field));
            ctx.t_fmt("popup.filter_by", &[("field", field_name)])
        }
        PopupKind::GoToRevision => ctx.t("popup.goto_title").to_string(),
        PopupKind::ChooseParent => ctx.t("popup.choose_parent_title").to_string(),
        PopupKind::CommitHistory => ctx.t("popup.commit_history_title").to_string(),
        PopupKind::ThreeWayInspector => ctx.t("popup.three_way_title").to_string(),
        PopupKind::ConflictMerge => format!(" {} ", ctx.t("menu.stage.resolve_conflicts")),
        PopupKind::HookError => ctx.t("popup.hook_error_title").to_string(),
    };

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
    // Block clicks inside popup from triggering backdrop close
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::ModalBackdrop { layer_z: z });

    match kind {
        PopupKind::PalettePreview => {
            render_palette_preview(frame, inner, ctx);
        }
        PopupKind::ShortcutsHelp => {
            if inner.width < 12 || inner.height < 4 {
                return;
            }
            let search_rect = Rect::new(inner.x, inner.y, inner.width, 1);
            let div_rect = Rect::new(inner.x, inner.y + 1, inner.width, 1);
            let list_h = inner.height.saturating_sub(3);
            let list_rect = Rect::new(inner.x, inner.y + 2, inner.width, list_h);
            let foot_rect = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);

            // Search bar header
            let prompt_prefix = " > ";
            let esc_hint = " [Esc] ";
            let avail_query_w = (inner.width as usize)
                .saturating_sub(prompt_prefix.width() + esc_hint.width() + 2);
            let mut search_spans: Vec<Span<'_>> = vec![Span::styled(
                prompt_prefix,
                Style::default()
                    .fg(ctx.theme.fg_accent_link)
                    .bg(ctx.theme.bg_panel)
                    .add_modifier(Modifier::BOLD),
            )];
            let mut used_search_w = prompt_prefix.width();
            if query.is_empty() {
                let placeholder =
                    truncate_display_width(ctx.t("help.search_placeholder"), avail_query_w);
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
            let search_gap =
                (inner.width as usize).saturating_sub(used_search_w + esc_hint.width());
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
                Paragraph::new(Line::from(search_spans))
                    .style(Style::default().bg(ctx.theme.bg_panel)),
                search_rect,
            );

            // Divider
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "─".repeat(inner.width as usize),
                    Style::default().fg(ctx.theme.border_splitter),
                )))
                .style(Style::default().bg(ctx.theme.bg_elevated)),
                div_rect,
            );

            // Command palette rows
            let rows = crate::keymap::build_help_rows(state, query);
            let resolved = crate::keymap::resolve_help_selection(&rows, selected);
            let show_scope = !query.trim().is_empty();

            if !rows
                .iter()
                .any(crate::keymap::HelpRow::is_selectable)
            {
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

                for (idx, row) in rows
                    .iter()
                    .enumerate()
                    .skip(scroll_y)
                    .take(vis_h)
                {
                    match row {
                        crate::keymap::HelpRow::Section(title) => {
                            lines.push(Line::from(Span::styled(
                                truncate_display_width(
                                    &format!(" {title}"),
                                    inner.width as usize,
                                ),
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
                            let desc_budget =
                                (inner.width as usize).saturating_sub(left_fixed + right_w);
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

            // Bottom hint bar
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
        PopupKind::FilterEditor(field) => {
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
                        ctx.i18n.map_or(field.hint(), |i| i.filter_field_hint(field))
                    ),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Line::from(Span::styled(
                    ctx.t("filter.editor_hint").to_string(),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ];
            let suggestions =
                crate::log_view::filter_suggestions(field, state.repo_snapshot.as_deref(), query);
            let room = inner.height.saturating_sub(3) as usize;
            let sel = selected.min(suggestions.len().saturating_sub(1));
            let start = sel.saturating_sub(room.saturating_sub(1));
            for (idx, s) in suggestions.iter().enumerate().skip(start).take(room) {
                let bg = if idx == sel {
                    ctx.theme.bg_selection_focused
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
        PopupKind::GoToRevision => {
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
        PopupKind::ChooseParent => {
            let mut lines = Vec::new();
            if let Some(commit) = state.log_commit_at(state.log_graph.selected_idx)
                && let Some(snap) = &state.repo_snapshot
            {
                let sel = selected.min(commit.parents.len().saturating_sub(1));
                for (idx, poid) in commit.parents.iter().enumerate() {
                    let subj = snap
                        .commits
                        .iter()
                        .find(|c| c.oid == *poid)
                        .map(|c| c.subject.as_str())
                        .unwrap_or_else(|| ctx.t("popup.choose_parent_outside"));
                    let bg = if idx == sel {
                        ctx.theme.bg_selection_focused
                    } else {
                        ctx.theme.bg_elevated
                    };
                    let prefix = format!(" {}. {} ", idx + 1, poid.short());
                    let max_subj = (inner.width as usize).saturating_sub(prefix.width());
                    lines.push(
                        Line::from(vec![
                            Span::styled(prefix, Style::default().fg(ctx.theme.fg_accent_link)),
                            Span::styled(
                                truncate_to_width(subj, max_subj),
                                Style::default().fg(ctx.theme.fg_primary),
                            ),
                        ])
                        .style(Style::default().bg(bg)),
                    );
                }
            }
            if lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    ctx.t("popup.choose_parent_none").to_string(),
                    Style::default().fg(ctx.theme.fg_muted),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        PopupKind::BranchesQuickView | PopupKind::ConflictMerge => {}
        PopupKind::CommitHistory => {
            let candidates = state.commit_history_candidates();
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
        PopupKind::ThreeWayInspector => {
            let mut lines = Vec::new();
            if let Some(diff) = &state.stage_workbench.current_diff
                && let Some(range) = state.stage_workbench.three_way_range_for_popup()
            {
                let max_w = inner.width.saturating_sub(4) as usize;
                let local_rows = diff.tracker.local_doc.slice_display(range.local_range);
                let staged_rows = diff.tracker.staged_doc.slice_display(range.staged_range);
                let vcs_rows = diff.tracker.vcs_doc.slice_display(range.vcs_range);

                let compute_ranges =
                    |primary: &[String], other_a: &[String], other_b: &[String]| {
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

                let mut push_section = |title: &str,
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
        PopupKind::HookError => {
            let msg = state
                .hook_error_output
                .as_deref()
                .unwrap_or_else(|| ctx.t("popup.hook_error_fallback"));
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
    }
}

/// Darkens the cells under a 1-cell drop shadow (right column + bottom row).
pub fn draw_shadow(frame: &mut Frame<'_>, screen: Rect, rect: Rect) {
    let shade = ratatui::style::Color::Rgb(14, 14, 16);
    let dim = ratatui::style::Color::Rgb(70, 72, 76);
    let buf = frame.buffer_mut();
    let sx = rect.right();
    if sx < screen.right() {
        for y in rect.y.saturating_add(1)..rect.bottom().saturating_add(1).min(screen.bottom()) {
            if let Some(cell) = buf.cell_mut((sx, y)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
    let sy = rect.bottom();
    if sy < screen.bottom() {
        for x in rect.x.saturating_add(1)..rect.right().min(screen.right()) {
            if let Some(cell) = buf.cell_mut((x, sy)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
}

/// Label spans with the mnemonic letter underlined (`C̲herry-Pick`).
pub fn mnemonic_spans(label: &str, mnemonic: Option<char>, style: Style) -> Vec<Span<'static>> {
    let Some(m) = mnemonic else {
        return vec![Span::styled(label.to_string(), style)];
    };
    let lower = m.to_ascii_lowercase();
    match label
        .char_indices()
        .find(|(_, c)| c.to_ascii_lowercase() == lower)
    {
        Some((pos, c)) => {
            let end = pos + c.len_utf8();
            vec![
                Span::styled(label[..pos].to_string(), style),
                Span::styled(
                    label[pos..end].to_string(),
                    style.add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(label[end..].to_string(), style),
            ]
        }
        None => vec![Span::styled(label.to_string(), style)],
    }
}

#[allow(clippy::too_many_arguments)]
fn render_context_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    anchor: (u16, u16),
    parent: Option<MenuParent>,
    items: &[ContextMenuItem],
    selected: usize,
    depth: u8,
    z: u8,
) {
    if items.is_empty() || screen.width < 12 || screen.height < 4 {
        return;
    }

    let rect = context_menu_rect(screen, anchor, parent, items);
    draw_shadow(frame, screen, rect);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(ctx.theme.border_splitter))
        .style(Style::default().bg(ctx.theme.bg_elevated));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    ctx.hit_registry
        .register(rect, z + 1, HitTarget::MenuBody { menu_depth: depth });

    let visible = inner.height as usize;
    let scroll = menu_scroll(selected, visible, items.len());
    let has_icons = menu_has_icons(items);

    for (vis, (idx, item)) in items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .enumerate()
    {
        let y = inner.y.saturating_add(vis as u16);
        if item.is_separator() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("├{}┤", "─".repeat(inner.width as usize)),
                    Style::default().fg(ctx.theme.border_splitter),
                ))
                .style(Style::default().bg(ctx.theme.bg_elevated)),
                Rect::new(rect.x, y, rect.width, 1),
            );
            continue;
        }
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        let is_sel = idx == selected;
        let enabled = item.is_enabled();
        let bg = if is_sel {
            if enabled {
                ctx.theme.bg_selection_focused
            } else {
                ctx.theme.bg_selection_unfocused
            }
        } else {
            ctx.theme.bg_elevated
        };
        let fg = if !enabled {
            ctx.theme.fg_muted
        } else if is_sel {
            ctx.theme.fg_bright
        } else {
            ctx.theme.fg_primary
        };

        let right_label = match &item.kind {
            ContextMenuEntryKind::Submenu(_) => "▸",
            _ => item.shortcut.as_deref().unwrap_or(""),
        };
        let icon_w = if has_icons { 2 } else { 0 };
        let avail = (inner.width as usize).saturating_sub(right_label.width() + 3 + icon_w);
        let label = truncate_to_width(&item.label, avail);
        let pad = (inner.width as usize)
            .saturating_sub(label.width() + right_label.width() + 2 + icon_w);

        let mut spans = vec![Span::raw(" ")];
        if has_icons {
            let icon = item.icon.unwrap_or(" ");
            let icon = if icon.width() <= 1 {
                format!("{icon:<1} ")
            } else {
                truncate_to_width(icon, 2)
            };
            spans.push(Span::styled(
                icon,
                Style::default().fg(if enabled {
                    ctx.theme.fg_accent_link
                } else {
                    ctx.theme.fg_muted
                }),
            ));
        }
        spans.extend(mnemonic_spans(
            &label,
            if enabled { item.mnemonic } else { None },
            Style::default().fg(fg),
        ));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(
            format!("{right_label} "),
            Style::default().fg(ctx.theme.fg_muted),
        ));

        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            z + 1,
            HitTarget::MenuItem {
                menu_depth: depth,
                item_idx: idx,
            },
        );
    }
    let arrow_style = Style::default().fg(ctx.theme.fg_accent_link);
    if scroll > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled("▴", arrow_style)),
            Rect::new(rect.x + rect.width / 2, rect.y, 1, 1),
        );
    }
    if scroll + visible < items.len() {
        frame.render_widget(
            Paragraph::new(Span::styled("▾", arrow_style)),
            Rect::new(rect.x + rect.width / 2, rect.bottom().saturating_sub(1), 1, 1),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_critical_dialog(
    frame: &mut Frame<'_>,
    screen: Rect,
    ctx: &mut RenderContext<'_>,
    title: &str,
    body: &str,
    confirm_label: &str,
    cancel_label: &str,
    selected_button: ModalButton,
    z: u8,
) {
    let width = screen.width.clamp(screen.width.min(28), 60);
    let height = screen.height.clamp(screen.height.min(7), 12);
    let x = screen
        .x
        .saturating_add(screen.width.saturating_sub(width) / 2);
    let y = screen
        .y
        .saturating_add(screen.height.saturating_sub(height) / 2);
    let rect = Rect::new(x, y, width, height);

    frame.render_widget(Clear, rect);

    let block = Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(ctx.theme.status_conflicted)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ctx.theme.status_conflicted))
        .style(Style::default().bg(ctx.theme.bg_elevated));

    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body_h = inner.height.saturating_sub(2);
    let body_rect = Rect::new(inner.x, inner.y, inner.width, body_h);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(ctx.theme.fg_primary)),
        body_rect,
    );

    if inner.height >= 2 {
        let btn_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        let btn0 = format!(" [{confirm_label}] ");
        let btn1 = format!(" [{cancel_label}] ");
        let btn0_rect = Rect::new(inner.x.saturating_add(1), btn_y, btn0.width() as u16, 1);
        let btn1_rect = Rect::new(
            btn0_rect
                .x
                .saturating_add(btn0_rect.width)
                .saturating_add(2),
            btn_y,
            btn1.width() as u16,
            1,
        );

        let s0 = if selected_button == ModalButton::Confirm {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };
        let s1 = if selected_button == ModalButton::Cancel {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.border_focused)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ctx.theme.fg_primary)
                .bg(ctx.theme.bg_panel)
        };

        frame.render_widget(Paragraph::new(Span::styled(btn0, s0)), btn0_rect);
        frame.render_widget(Paragraph::new(Span::styled(btn1, s1)), btn1_rect);
        ctx.hit_registry.register(
            btn0_rect,
            z + 1,
            HitTarget::ModalButton(ModalButton::Confirm),
        );
        ctx.hit_registry.register(
            btn1_rect,
            z + 1,
            HitTarget::ModalButton(ModalButton::Cancel),
        );
    }
}
