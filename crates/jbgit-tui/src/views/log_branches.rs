//! Left pane: Branches Dashboard tree (folders, ★ favorites, `↑N ↓M`,
//! speed search) plus the collapsed `»` stripe.

use jbgit_core::{BranchCategory, BranchNodeKind, BranchTreeNodeId, PaneId, RefKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{BranchesToolbarButton, HitTarget};
use crate::log_view::FilterField;
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, row_background, truncate_to_width,
};

/// Collapsed pane: a 2-column clickable stripe (`ExpandStripeButton`).
pub fn render_branches_stripe(frame: &mut Frame<'_>, area: Rect, ctx: &mut RenderContext<'_>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hovered = matches!(ctx.hover_target, Some(HitTarget::BranchesExpandStripe));
    let bg = if hovered {
        ctx.theme.bg_hover
    } else {
        ctx.theme.bg_panel
    };
    let mut lines = vec![Line::from(Span::styled(
        "» ",
        Style::default()
            .fg(ctx.theme.fg_bright)
            .add_modifier(Modifier::BOLD),
    ))];
    for ch in ctx
        .t("view.branches.stripe")
        .chars()
        .take(area.height.saturating_sub(2) as usize)
    {
        lines.push(Line::from(Span::styled(
            format!("{ch} "),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(bg)), area);
    ctx.hit_registry
        .register(area, 2, HitTarget::BranchesExpandStripe);
}

pub fn render_branches_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let pane = &state.branches;
    let sub = state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            state
                .i18n
                .t_fmt("view.branches.refs_count", &[("count", &s.refs.len().to_string())])
        })
        .unwrap_or_default();
    let body = render_pane_frame(frame, area, PaneId::LogBranchesTree, &sub, ctx);

    // Header toolbar: [⌖] locate, [⊞] group by directory, [«] collapse.
    if area.width >= 20 {
        let buttons = [
            (" ⌖", BranchesToolbarButton::Locate),
            (" ⊞", BranchesToolbarButton::GroupByDir),
            (" «", BranchesToolbarButton::Collapse),
        ];
        let total: u16 = buttons.iter().map(|(t, _)| t.width() as u16).sum::<u16>() + 1;
        let mut x = area.right().saturating_sub(total);
        for (text, btn) in buttons {
            let w = text.width() as u16;
            let rect = Rect::new(x, area.y, w, 1);
            let active = btn == BranchesToolbarButton::GroupByDir
                && pane.settings.options.group_by_directory;
            let hovered =
                matches!(ctx.hover_target, Some(HitTarget::BranchesToolbar(b)) if *b == btn);
            let fg = if active || hovered {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_muted
            };
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::default().fg(fg))),
                rect,
            );
            ctx.hit_registry
                .register(rect, 3, HitTarget::BranchesToolbar(btn));
            x += w;
        }
    }

    if body.width == 0 || body.height == 0 {
        return;
    }
    let mut list_area = body;
    if !state.log_filter.spec.branch.is_empty() && list_area.height > 1 {
        let bar = Rect::new(list_area.x, list_area.y, list_area.width, 1);
        let btn_text = state.i18n.t("view.branches.clear_filter_btn");
        let label = state.i18n.t_fmt(
            "view.branches.filtered_prefix",
            &[("label", &state.log_filter.spec.branch.label())],
        );
        let btn_w = btn_text.width() as u16;
        let left_w = list_area.width.saturating_sub(btn_w + 1) as usize;
        let hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::FilterClear(FilterField::Branch))
        );
        let btn_style = if hovered {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_focused)
        } else {
            Style::default()
                .fg(ctx.theme.fg_accent_link)
                .bg(ctx.theme.bg_panel)
        };
        let line = Line::from(vec![
            Span::styled(
                truncate_to_width(&label, left_w),
                Style::default()
                    .fg(ctx.theme.fg_muted)
                    .bg(ctx.theme.bg_panel),
            ),
            Span::styled(
                " ".repeat(left_w.saturating_sub(truncate_to_width(&label, left_w).width()) + 1),
                Style::default().bg(ctx.theme.bg_panel),
            ),
            Span::styled(btn_text, btn_style),
        ]);
        frame.render_widget(Paragraph::new(line), bar);
        if list_area.width >= btn_w {
            let btn_rect = Rect::new(bar.right().saturating_sub(btn_w), bar.y, btn_w, 1);
            ctx.hit_registry
                .register(btn_rect, 3, HitTarget::FilterClear(FilterField::Branch));
        }
        list_area = Rect::new(
            list_area.x,
            list_area.y + 1,
            list_area.width,
            list_area.height.saturating_sub(1),
        );
    }
    if !pane.query.is_empty() {
        let bar = Rect::new(list_area.x, list_area.y, list_area.width, 1);
        let matches = pane.rows.iter().filter(|r| r.expanded.is_none()).count();
        let text = format!(" ⌕ {}  ({matches})", pane.query);
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_to_width(&text, list_area.width as usize),
                Style::default().fg(ctx.theme.fg_bright),
            ))
            .style(Style::default().bg(ctx.theme.bg_selection_focused)),
            bar,
        );
        list_area = Rect::new(
            list_area.x,
            list_area.y + 1,
            list_area.width,
            list_area.height.saturating_sub(1),
        );
    }
    if list_area.height == 0 {
        return;
    }

    if state.repo_snapshot.is_none() {
        return;
    }
    if pane.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                if pane.query.is_empty() {
                    state.i18n.t("view.branches.no_branches")
                } else {
                    state.i18n.t("view.branches.no_matching")
                },
                Style::default().fg(ctx.theme.fg_muted),
            )),
            list_area,
        );
        return;
    }

    let focused = ctx.focused_pane == PaneId::LogBranchesTree;
    let glyphs = ctx.glyph_mode.symbols();
    let visible_h = list_area.height as usize;
    let (selected, offset) = list_viewport(pane.rows.len(), pane.selected, visible_h);
    let filter_names = &state.log_filter.spec.branch.include;

    for (vis_y, row_idx) in (offset..pane.rows.len()).take(visible_h).enumerate() {
        let row = pane.rows[row_idx];
        let node = &pane.model.nodes[row.node];
        let row_rect = Rect::new(list_area.x, list_area.y + vis_y as u16, list_area.width, 1);
        let is_sel = row_idx == selected;
        let in_multi = pane.multi.contains(&node.key);
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::BranchTreeNode { row: h_row, .. }) if *h_row == row_idx
        );
        let mut bg = row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);
        if in_multi && !is_sel {
            bg = ctx.theme.bg_selection_unfocused;
        }

        let indent = "  ".repeat(row.depth as usize);
        let mut spans: Vec<Span<'static>> = vec![Span::raw(format!(" {indent}"))];
        let mut right: Vec<Span<'static>> = Vec::new();

        match (node.kind, row.expanded) {
            (_, Some(expanded)) => {
                let arrow = if expanded {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                let arrow_x = row_rect.x + 1 + indent.width() as u16;
                ctx.hit_registry.register(
                    Rect::new(arrow_x, row_rect.y, arrow.width() as u16 + 1, 1),
                    3,
                    HitTarget::BranchTreeToggle { row: row_idx },
                );
                spans.push(Span::styled(
                    format!("{arrow} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                let (label_style, show_count) = match node.kind {
                    BranchNodeKind::Category(_) => (
                        Style::default()
                            .fg(ctx.theme.fg_primary)
                            .add_modifier(Modifier::BOLD),
                        true,
                    ),
                    BranchNodeKind::RemoteGroup => {
                        (Style::default().fg(ctx.theme.ref_remote.icon), false)
                    }
                    _ => (Style::default().fg(ctx.theme.fg_primary), false),
                };
                if node.kind == BranchNodeKind::Folder {
                    spans.push(Span::styled(
                        format!("{} ", glyphs.files.folder),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
                let cat_label = if let BranchNodeKind::Category(cat) = node.kind {
                    ctx.branch_category_title(cat).to_string()
                } else {
                    node.label.clone()
                };
                spans.push(Span::styled(cat_label, label_style));
                if show_count || node.kind == BranchNodeKind::Category(BranchCategory::Tags) {
                    spans.push(Span::styled(
                        format!(" ({})", node.leaf_count),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
            }
            (BranchNodeKind::Leaf(kind), None) => {
                let (icon, icon_fg) = match kind {
                    RefKind::Head => (glyphs.head, ctx.theme.ref_head.icon),
                    RefKind::LocalBranch if node.is_current => {
                        (glyphs.head, ctx.theme.ref_head.icon)
                    }
                    RefKind::LocalBranch => (glyphs.local_branch, ctx.theme.ref_local.icon),
                    RefKind::RemoteBranch => (glyphs.remote_branch, ctx.theme.ref_remote.icon),
                    RefKind::Tag => (glyphs.tag, ctx.theme.ref_tag.icon),
                };
                spans.push(Span::styled(
                    if node.is_favorite {
                        format!("{} ", glyphs.favorite_star)
                    } else {
                        "  ".to_string()
                    },
                    Style::default().fg(ctx.theme.ref_head.icon),
                ));
                spans.push(Span::styled(
                    format!("{icon} "),
                    Style::default().fg(icon_fg),
                ));
                let in_filter = node
                    .ref_name
                    .as_ref()
                    .is_some_and(|n| filter_names.iter().any(|f| f == n));
                let name_fg = if in_filter {
                    ctx.theme.fg_accent_link
                } else if is_sel || node.is_current {
                    ctx.theme.fg_bright
                } else if kind == RefKind::Tag {
                    ctx.theme.fg_muted
                } else {
                    ctx.theme.fg_primary
                };
                let mut style = Style::default().fg(name_fg);
                if node.is_current || kind == RefKind::Head {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let leaf_label = if kind == RefKind::Head {
                    if let Some(oid_str) = node
                        .label
                        .strip_prefix("HEAD (detached ")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        state
                            .i18n
                            .t_fmt("view.branches.head_detached", &[("oid", oid_str)])
                    } else {
                        node.label.clone()
                    }
                } else {
                    node.label.clone()
                };
                spans.push(Span::styled(leaf_label, style));
                if let Some((ahead, behind)) = node.ahead_behind {
                    if ahead > 0 {
                        right.push(Span::styled(
                            format!(" {}{ahead}", glyphs.arrow_up),
                            Style::default().fg(ctx.theme.status_added),
                        ));
                    }
                    if behind > 0 {
                        right.push(Span::styled(
                            format!(" {}{behind}", glyphs.arrow_down),
                            Style::default().fg(ctx.theme.fg_accent_link),
                        ));
                    }
                }
            }
            _ => {}
        }

        // Truncate the left part so right-aligned sync badges stay visible.
        let line_spans = fit_left_right_spans(spans, right, row_rect.width as usize, 1);
        frame.render_widget(
            Paragraph::new(Line::from(line_spans)).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            2,
            HitTarget::BranchTreeNode {
                pane: PaneId::LogBranchesTree,
                node_id: BranchTreeNodeId(row.node as u32),
                row: row_idx,
            },
        );
    }
}
