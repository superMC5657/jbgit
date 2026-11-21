use jbgit_core::{ChangeRowKind, ChangeTreeRow, CommitChangesBundle, DetailsMode, PaneId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{ChangesToolbarButton, HitTarget};
use crate::log_view::{ChangesExpandState, DetailsPaneState};
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, render_toolbar, row_background,
};

use super::{DiffHost, muted_line};

pub fn render_changes_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    render_changes_browser(
        frame,
        area,
        &state.details,
        DiffHost::Log,
        PaneId::LogChangesTree,
        ctx,
    );
}

pub fn render_changes_browser(
    frame: &mut Frame<'_>,
    area: Rect,
    details: &DetailsPaneState,
    host: DiffHost,
    pane: PaneId,
    ctx: &mut RenderContext<'_>,
) {
    let bundle = details.current_bundle();
    let subtitle = match (bundle, &details.current_key) {
        (Some(b), _) => {
            let (add, del) = b.totals();
            let stats = ctx.t_fmt(
                "view.log_details.files_stats",
                &[
                    ("count", &b.changes.len().to_string()),
                    ("add", &add.to_string()),
                    ("del", &del.to_string()),
                ],
            );
            match b.key.mode {
                DetailsMode::CompareVersions if b.key.oids.len() >= 2 => ctx.t_fmt(
                    "view.log_details.compare_versions_sub",
                    &[
                        ("a", &b.key.oids[0].short()),
                        ("b", &b.key.oids[1].short()),
                        ("stats", &stats),
                    ],
                ),
                DetailsMode::CompareWithLocal if !b.key.oids.is_empty() => ctx.t_fmt(
                    "view.log_details.compare_local_sub",
                    &[("a", &b.key.oids[0].short()), ("stats", &stats)],
                ),
                _ if b.key.oids.len() > 1 => ctx.t_fmt(
                    "view.log_details.commits_stats_sub",
                    &[
                        ("count", &b.key.oids.len().to_string()),
                        ("stats", &stats),
                    ],
                ),
                _ => stats,
            }
        }
        (None, Some(_)) => ctx.t("common.loading").to_string(),
        (None, None) => String::new(),
    };
    let body = render_pane_frame(frame, area, pane, &subtitle, ctx);
    if body.height == 0 || body.width == 0 {
        return;
    }

    let merge_parents = bundle
        .filter(|b| b.key.mode == DetailsMode::Commits && b.key.oids.len() == 1)
        .and_then(|b| b.details.first())
        .map_or(0, |d| d.parents.len());
    let expand_state = details.changes_expand_state();
    let mut buttons = vec![
        (
            ctx.t("view.log_details.toolbar_dir").to_string(),
            details.changes_group_by_dir,
            ChangesToolbarButton::GroupByDir,
        ),
        (
            " ⊕ ".to_string(),
            expand_state == ChangesExpandState::AllExpanded,
            ChangesToolbarButton::ExpandAll,
        ),
        (
            " ⊖ ".to_string(),
            expand_state == ChangesExpandState::AllCollapsed,
            ChangesToolbarButton::CollapseAll,
        ),
    ];
    if host == DiffHost::Log {
        buttons.push((
            ctx.t("view.log_details.toolbar_diff").to_string(),
            details.diff_preview,
            ChangesToolbarButton::DiffPreview,
        ));
        if merge_parents >= 2 {
            buttons.push((
                ctx.t_fmt(
                    "view.log_details.toolbar_parent",
                    &[
                        ("cur", &(details.merge_parent + 1).to_string()),
                        ("total", &merge_parents.to_string()),
                    ],
                ),
                true,
                ChangesToolbarButton::SwitchParent,
            ));
        }
    }
    render_toolbar(
        frame,
        Rect::new(body.x, body.y, body.width, 1),
        ctx,
        3,
        &buttons,
        |b| match host {
            DiffHost::Log => HitTarget::ChangesToolbar(b),
            DiffHost::Stash => HitTarget::StashChangesToolbar(b),
        },
    );

    let list = Rect::new(
        body.x,
        body.y + 1,
        body.width,
        body.height.saturating_sub(1),
    );
    if list.height == 0 {
        return;
    }
    let Some(bundle) = bundle else {
        let msg = if details.current_key.is_some() {
            ctx.t("view.log_details.loading_changes")
        } else {
            ctx.t("view.log_details.select_commit_changes")
        };
        frame.render_widget(muted_line(ctx, msg), list);
        return;
    };
    if bundle.changes.is_empty() {
        frame.render_widget(muted_line(ctx, ctx.t("view.log_details.no_changes")), list);
        return;
    }
    let rows = details.change_rows();
    render_change_rows(frame, list, details, host, pane, ctx, bundle, &rows);
}

#[allow(clippy::too_many_arguments)]
fn render_change_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    details: &DetailsPaneState,
    host: DiffHost,
    pane: PaneId,
    ctx: &mut RenderContext<'_>,
    bundle: &CommitChangesBundle,
    rows: &[ChangeTreeRow],
) {
    let focused = ctx.focused_pane == pane;
    let glyphs = ctx.glyph_mode.symbols();
    let (selected, offset) = list_viewport(rows.len(), details.changes_selected, area.height as usize);
    for (vis_y, idx) in (offset..rows.len()).take(area.height as usize).enumerate() {
        let row = &rows[idx];
        let rect = Rect::new(area.x, area.y + vis_y as u16, area.width, 1);
        let row_target = match host {
            DiffHost::Log => HitTarget::LogChangeRow { idx },
            DiffHost::Stash => HitTarget::StashChangeRow(idx),
        };
        let hovered = ctx.hover_target == Some(&row_target);
        let bg = row_background(
            idx == selected,
            focused,
            hovered,
            ctx.theme.bg_canvas,
            ctx.theme,
        );
        let indent = "  ".repeat(row.depth as usize);
        let mut left: Vec<Span<'static>> = vec![Span::raw(format!(" {indent}"))];
        let mut right: Vec<Span<'static>> = Vec::new();
        match row.kind {
            ChangeRowKind::Dir { file_count } => {
                let arrow = if row.expanded {
                    glyphs.tree_expanded
                } else {
                    glyphs.tree_collapsed
                };
                left.push(Span::styled(
                    format!("{arrow} "),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(
                    format!("{} ", glyphs.files.folder),
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
            }
            ChangeRowKind::File { change_idx } => {
                let change = &bundle.changes[change_idx];
                let color = ctx.theme.status_color(change.kind);
                left.push(Span::styled(
                    format!("{} ", change.kind.badge_char()),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
                left.push(Span::styled(
                    format!("{} ", glyphs.file_icon(&change.path)),
                    Style::default().fg(ctx.theme.fg_muted),
                ));
                left.push(Span::styled(row.label.clone(), Style::default().fg(color)));
                if let Some(old) = &change.old_path {
                    left.push(Span::styled(
                        format!(" ← {}", old.rsplit('/').next().unwrap_or(old)),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
                if change.is_binary {
                    right.push(Span::styled(
                        ctx.t("view.log_details.bin_badge").to_string(),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                } else {
                    right.push(Span::styled(
                        format!("+{}", change.additions),
                        Style::default().fg(ctx.theme.status_added),
                    ));
                    right.push(Span::styled(
                        format!(" -{} ", change.deletions),
                        Style::default().fg(ctx.theme.status_conflicted),
                    ));
                }
            }
        }
        let spans = fit_left_right_spans(left, right, rect.width as usize, 1);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            rect,
        );
        ctx.hit_registry.register(rect, 2, row_target);
    }
}
