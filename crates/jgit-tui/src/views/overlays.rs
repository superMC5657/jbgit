use jgit_core::{GroupOptions, ModalButton, RefInfo, group_refs_for_table, word_diff};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    AppState, ContextMenuEntryKind, ContextMenuItem, MenuParent, ModalLayer, PopupKind,
    RenderContext,
};
use crate::hit_test::HitTarget;
use crate::syntax::render_diff_line_spans;
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
                " Detached HEAD ",
                ctx.theme.ref_detached.icon,
                vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", g.detached_head),
                            Style::default().fg(ctx.theme.ref_detached.icon),
                        ),
                        Span::styled(
                            "HEAD points directly to a commit (not a branch).",
                            Style::default().fg(ctx.theme.ref_detached.pill_fg),
                        ),
                    ]),
                    Line::from(Span::styled(
                        " New commits will not belong to any branch until checked out.",
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
                " Right-click +N for actions",
                Style::default().fg(ctx.theme.fg_muted),
            )));
            (
                " All Commit References ",
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
        PopupKind::ShortcutsHelp => (78, 34),
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
        PopupKind::ShortcutsHelp => " Keyboard & Mouse Shortcuts (Esc to close) ".to_string(),
        PopupKind::BranchesQuickView => " Git Branches (Type to filter, Esc to close) ".to_string(),
        PopupKind::PalettePreview => " Darcula Palette Preview ".to_string(),
        PopupKind::FilterEditor(field) => format!(" Filter by {} ", field.title()),
        PopupKind::GoToRevision => " Go to Hash / Branch / Tag ".to_string(),
        PopupKind::ChooseParent => " Choose Parent Commit (1-9 / Enter) ".to_string(),
        PopupKind::CommitHistory => " Commit Message History (Enter to fill, Esc to close) ".to_string(),
        PopupKind::ThreeWayInspector => " Three-Way Change Inspector (Worktree / Staged / HEAD) ".to_string(),
        PopupKind::ConflictMerge => " Resolve Conflicts ".to_string(),
        PopupKind::HookError => " Commit / Git Hook Failed (Message Preserved) ".to_string(),
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
            const HELP: &[(&str, &str)] = &[
                ("1 / 2 / 3 / 4", "Switch Commit / Log / Stash / Console"),
                (
                    "Tab / Shift+Tab",
                    "Cycle Branches → Commits → Changes → Details",
                ),
                ("Mouse drag │ / ─", "Resize splitters (double-click resets)"),
                ("Right click", "Pre-select item & open context menu"),
                ("r / p", "Refresh snapshot / Run git status probe"),
                ("F12", "Toggle Unicode / NerdFont glyphs"),
                ("", ""),
                ("── Log ──", ""),
                (
                    "Ctrl+L or /",
                    "Text / hash filter (Alt+R regex, Alt+C case)",
                ),
                ("Alt+B", "Collapse / expand Branches pane"),
                (
                    "Branches: type",
                    "Speed search · Esc clears / drops branch filter",
                ),
                (
                    "Branches: Enter",
                    "Filter log by branch / folder (Ctrl+Click = union)",
                ),
                (
                    "Branches: J / Space",
                    "Navigate log to tip / toggle ★ favorite",
                ),
                ("Shift+Up/Down", "Extend commit selection (net changes)"),
                ("→ / Shift+→ / ←", "Go to parent / second parent / child"),
                ("Ctrl+G", "Go to hash, branch, tag or HEAD~N"),
                ("Alt+← / Alt+→", "Navigation history back / forward"),
                ("g G Ctrl+D/U PgUp/Dn", "Top / bottom / half page / page"),
                ("Changes: Enter", "Toggle inline diff preview (Ctrl+D)"),
                ("Changes: F4", "Full-screen diff · [ ] prev/next file"),
                ("Diff: s / c / m", "Split / context 3→10→Full / dock"),
                ("Commits: P", "Choose parent commit on merges"),
                (
                    "v / e / E / P",
                    "Dir grouping / expand / collapse / merge parent",
                ),
                ("Details: click SHA", "Copy 40-char commit SHA (OSC 52)"),
                ("", ""),
                ("── Branches & menus ──", ""),
                ("Ctrl+` / click ⎇ pill", "Git Branches popup (Alt+B outside Log)"),
                ("Popup: type / ↑↓ / → ", "Fuzzy search · move · branch actions"),
                ("Shift+F10 / Menu / .", "Context menu for the focused row (m too)"),
                ("Ctrl+Alt+N", "New branch from HEAD"),
                ("Alt+Z / Alt+X", "Run / dismiss footer notification action"),
                ("Banner buttons", "Resolve · Continue · Skip · Abort"),
                ("Stash: a p b d n C", "Apply · Pop · Branch · Drop · New · Clear"),
                ("q / Ctrl+C", "Quit"),
            ];
            let lines: Vec<Line<'_>> = HELP
                .iter()
                .map(|(k, v)| {
                    Line::from(vec![
                        Span::styled(
                            format!(" {k:<22}"),
                            Style::default().fg(ctx.theme.fg_bright),
                        ),
                        Span::styled(*v, Style::default().fg(ctx.theme.fg_primary)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
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
                    format!(" {}", field.hint()),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
                Line::from(Span::styled(
                    " Enter apply · Tab complete · ↑↓ pick · empty = all · Esc cancel",
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
                    " No matching revision",
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
                    " SHA prefix, branch, tag, HEAD~N, main^2 · Enter jump · Esc cancel",
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
                        .unwrap_or("(commit outside loaded snapshot)");
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
                    " No parent commits",
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
                Span::styled(" Filter: ", Style::default().fg(ctx.theme.fg_muted)),
                Span::styled(
                    format!("{query}▏"),
                    Style::default().fg(ctx.theme.fg_bright),
                ),
            ])];
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    " No commit history yet",
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
                            "   (empty)",
                            Style::default().fg(ctx.theme.fg_muted),
                        )));
                    } else {
                        let base = Style::default().fg(ctx.theme.fg_primary);
                        for (idx, r) in rows.iter().enumerate() {
                            let wr = word_ranges.get(idx).map(Vec::as_slice).unwrap_or(&[]);
                            let mut spans = vec![Span::raw("   ")];
                            spans.extend(render_diff_line_spans(
                                r,
                                &[],
                                wr,
                                max_w,
                                base,
                                word_bg,
                                ctx.theme.true_color,
                            ));
                            lines.push(Line::from(spans));
                        }
                    }
                };
                push_section(
                    "Worktree (Unstaged)",
                    ctx.theme.status_added,
                    ctx.theme.diff_add_word_bg,
                    &local_rows,
                    &local_word_ranges,
                );
                push_section(
                    "Staged (Index)",
                    ctx.theme.status_modified,
                    ctx.theme.diff_mod_bg,
                    &staged_rows,
                    &staged_word_ranges,
                );
                push_section(
                    "HEAD (Base)",
                    ctx.theme.fg_muted,
                    ctx.theme.diff_del_word_bg,
                    &vcs_rows,
                    &vcs_word_ranges,
                );
            } else {
                lines.push(Line::from(Span::styled(
                    " Move cursor onto a changed line to inspect HEAD / Staged / Worktree",
                    Style::default().fg(ctx.theme.fg_muted),
                )));
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        PopupKind::HookError => {
            let msg = state
                .hook_error_output
                .as_deref()
                .unwrap_or("Commit failed");
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
                " Your commit message was preserved. Press Esc to close.",
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
