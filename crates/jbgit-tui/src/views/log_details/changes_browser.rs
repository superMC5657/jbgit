use jbgit_core::{ChangeRowKind, ChangeTreeRow, CommitChangesBundle, DetailsMode, PaneId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::diff_session::{ChangesExpandState, ChangesTreeSession, DiffCacheStore};
use crate::hit_test::{ChangesToolbarButton, HitTarget};
use crate::log_view::MAX_AGGREGATED_COMMITS;
use crate::views::{
    fit_left_right_spans, list_viewport, render_pane_frame, render_toolbar, row_background,
    wrap_text,
};

use super::{DiffHost, muted_line};

pub struct ChangesBrowserProps<'a> {
    pub changes: &'a ChangesTreeSession,
    pub caches: &'a DiffCacheStore,
    pub host: DiffHost,
    pub pane: PaneId,
    pub diff_preview: bool,
    pub merge_parent: usize,
    pub many_changes_count: Option<usize>,
    pub path_filter_active: bool,
    pub show_only_affected: bool,
}

pub fn render_changes_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    render_changes_browser(
        frame,
        area,
        ChangesBrowserProps {
            changes: &state.log.details.changes,
            caches: &state.diff_caches,
            host: DiffHost::Log,
            pane: PaneId::LogChangesTree,
            diff_preview: state.log.details.diff_preview,
            merge_parent: state.log.details.merge_parent,
            many_changes_count: state
                .log
                .details
                .many_changes_blocked
                .as_ref()
                .map(|k| k.oids.len()),
            path_filter_active: !state.log.filter.spec.paths.is_empty(),
            show_only_affected: state.log.presentation.show_only_affected_changes,
        },
        ctx,
    );
}

pub fn render_changes_browser(
    frame: &mut Frame<'_>,
    area: Rect,
    props: ChangesBrowserProps<'_>,
    ctx: &mut RenderContext<'_>,
) {
    let ChangesBrowserProps {
        changes,
        caches,
        host,
        pane,
        diff_preview,
        merge_parent,
        many_changes_count,
        path_filter_active,
        show_only_affected,
    } = props;
    let bundle = changes.current_bundle(caches);
    let counts = changes.visible_change_counts(caches);
    let subtitle = if let Some(count) = many_changes_count {
        ctx.t_fmt(
            "view.log_details.many_changes_sub",
            &[
                ("count", &count.to_string()),
                ("limit", &MAX_AGGREGATED_COMMITS.to_string()),
            ],
        )
    } else {
        match (bundle, &changes.current_key) {
            (Some(b), _) => {
                let (shown, _) = counts.unwrap_or((b.changes.len(), b.changes.len()));
                let (add, del) = changes.visible_totals(caches);
                let stats = ctx.t_fmt(
                    "view.log_details.files_stats",
                    &[
                        ("count", &shown.to_string()),
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
                    DetailsMode::CompareWithLocal if !b.key.oids.is_empty() => {
                        match b.key.path_prefix.as_deref() {
                            Some(dir) => ctx.t_fmt(
                                "view.log_details.compare_local_dir_sub",
                                &[
                                    ("a", &b.key.oids[0].short()),
                                    ("dir", dir),
                                    ("stats", &stats),
                                ],
                            ),
                            None => ctx.t_fmt(
                                "view.log_details.compare_local_sub",
                                &[("a", &b.key.oids[0].short()), ("stats", &stats)],
                            ),
                        }
                    }
                    _ if b.key.oids.len() > 1 => ctx.t_fmt(
                        "view.log_details.commits_stats_sub",
                        &[("count", &b.key.oids.len().to_string()), ("stats", &stats)],
                    ),
                    _ => stats,
                }
            }
            (None, Some(_)) => ctx.t("common.loading").to_string(),
            (None, None) => String::new(),
        }
    };
    let body = render_pane_frame(frame, area, pane, &subtitle, ctx);

    // Clickable `[Filtered: M/N]` badge on the Changes pane title bar when PathFilter + Show Only Affected is active.
    if host == DiffHost::Log
        && path_filter_active
        && show_only_affected
        && let Some((shown, total)) = counts
        && area.height > 0
    {
        let badge = ctx.t_fmt(
            "view.log_details.filtered_badge",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        );
        let badge_w = badge.width() as u16;
        if badge_w > 0 && area.width > badge_w + 10 {
            let badge_rect = Rect::new(area.right().saturating_sub(badge_w), area.y, badge_w, 1);
            let hovered = ctx.hover_target == Some(&HitTarget::ChangesFilteredBadge);
            let style = Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(if hovered {
                    ctx.theme.border_focused
                } else {
                    ctx.theme.bg_selection_unfocused
                })
                .add_modifier(Modifier::BOLD);
            frame.render_widget(Paragraph::new(Span::styled(badge, style)), badge_rect);
            ctx.hit_registry
                .register(badge_rect, 3, HitTarget::ChangesFilteredBadge);
        }
    }

    if body.height == 0 || body.width == 0 {
        return;
    }

    let merge_parents = bundle
        .filter(|b| b.key.mode == DetailsMode::Commits && b.key.oids.len() == 1)
        .and_then(|b| b.details.first())
        .map_or(0, |d| d.parents.len());
    let is_compare_versions = changes
        .current_key
        .as_ref()
        .is_some_and(|k| k.mode == DetailsMode::CompareVersions && k.oids.len() >= 2);
    let expand_state = changes.expand_state(caches);
    let mut buttons = vec![
        (
            ctx.t("view.log_details.toolbar_dir").to_string(),
            changes.group_by_dir,
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
            diff_preview,
            ChangesToolbarButton::DiffPreview,
        ));
        if is_compare_versions {
            buttons.push((
                ctx.t("view.log_details.toolbar_swap").to_string(),
                false,
                ChangesToolbarButton::SwapCompare,
            ));
        }
        if path_filter_active {
            let lbl = if show_only_affected {
                if let Some((shown, total)) = counts {
                    ctx.t_fmt(
                        "view.log_details.toolbar_affected_count",
                        &[("shown", &shown.to_string()), ("total", &total.to_string())],
                    )
                } else {
                    ctx.t("view.log_details.toolbar_affected").to_string()
                }
            } else {
                ctx.t("view.log_details.toolbar_affected").to_string()
            };
            buttons.push((lbl, show_only_affected, ChangesToolbarButton::OnlyAffected));
        }
        if merge_parents >= 2 {
            buttons.push((
                ctx.t_fmt(
                    "view.log_details.toolbar_parent",
                    &[
                        ("cur", &(merge_parent + 1).to_string()),
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
    if let Some(count) = many_changes_count {
        let prompt = ctx.t_fmt(
            "view.log_details.many_changes_prompt",
            &[
                ("count", &count.to_string()),
                ("limit", &MAX_AGGREGATED_COMMITS.to_string()),
            ],
        );
        let hovered = ctx.hover_target == Some(&HitTarget::ChangesLoadMany);
        let style = Style::default()
            .fg(if hovered {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_accent_link
            })
            .bg(if hovered {
                ctx.theme.bg_hover
            } else {
                ctx.theme.bg_canvas
            })
            .add_modifier(Modifier::BOLD);
        let wrapped = wrap_text(
            prompt.trim_start(),
            list.width.saturating_sub(2).max(1) as usize,
        );
        let lines: Vec<Line<'static>> = wrapped
            .into_iter()
            .take(list.height as usize)
            .map(|l| Line::from(Span::styled(format!(" {l}"), style)))
            .collect();
        let prompt_h = (lines.len() as u16).max(1).min(list.height);
        let prompt_rect = Rect::new(list.x, list.y, list.width, prompt_h);
        frame.render_widget(Paragraph::new(lines), prompt_rect);
        ctx.hit_registry
            .register(prompt_rect, 2, HitTarget::ChangesLoadMany);
        return;
    }
    let Some(bundle) = bundle else {
        let msg = if changes.current_key.is_some() {
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
    let rows = changes.change_rows(caches);
    if rows.is_empty() && path_filter_active && show_only_affected {
        let msg = ctx.t_fmt(
            "view.log_details.no_affected_changes",
            &[("total", &bundle.changes.len().to_string())],
        );
        let hovered = ctx.hover_target == Some(&HitTarget::ChangesFilteredBadge);
        let style = Style::default()
            .fg(if hovered {
                ctx.theme.fg_bright
            } else {
                ctx.theme.fg_accent_link
            })
            .bg(if hovered {
                ctx.theme.bg_hover
            } else {
                ctx.theme.bg_canvas
            });
        let wrapped = wrap_text(
            msg.trim_start(),
            list.width.saturating_sub(2).max(1) as usize,
        );
        let lines: Vec<Line<'static>> = wrapped
            .into_iter()
            .take(list.height as usize)
            .map(|l| Line::from(Span::styled(format!(" {l}"), style)))
            .collect();
        let msg_h = (lines.len() as u16).max(1).min(list.height);
        let msg_rect = Rect::new(list.x, list.y, list.width, msg_h);
        frame.render_widget(Paragraph::new(lines), msg_rect);
        ctx.hit_registry
            .register(msg_rect, 2, HitTarget::ChangesFilteredBadge);
        return;
    }
    render_change_rows_inner(
        frame,
        list,
        ChangeRowsView {
            selected_idx: changes.selected,
            host,
            pane,
            bundle,
            rows: &rows,
        },
        ctx,
    );
}

struct ChangeRowsView<'a> {
    selected_idx: usize,
    host: DiffHost,
    pane: PaneId,
    bundle: &'a CommitChangesBundle,
    rows: &'a [ChangeTreeRow],
}

fn render_change_rows_inner(
    frame: &mut Frame<'_>,
    area: Rect,
    view: ChangeRowsView<'_>,
    ctx: &mut RenderContext<'_>,
) {
    let ChangeRowsView {
        selected_idx,
        host,
        pane,
        bundle,
        rows,
    } = view;
    let focused = ctx.focused_pane == pane;
    let glyphs = ctx.glyph_mode.symbols();
    let (selected, offset) = list_viewport(rows.len(), selected_idx, area.height as usize);
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
