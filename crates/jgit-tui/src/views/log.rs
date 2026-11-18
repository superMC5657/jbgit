use jgit_core::graph::{GraphPrintElement, PrintElementGenerator, ReachableNodes, VisibleRowIdx};
use jgit_core::{
    BranchTreeNodeId, GroupOptions, Oid, PaneId, RefInfo, RefKind, RepoSnapshot, SplitAxis,
    SplitterId, group_refs_for_table,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::graph::{
    CellGridGraphBackend, GraphRenderBackend, GraphViewportSlice, RenderGraphElement, RowGraphSlice,
};
use crate::hit_test::{GraphHitKind, HitTarget};
use crate::views::{render_pane_frame, render_splitter_divider, row_background, truncate_to_width};
use crate::widgets::PillBadgeBar;

pub fn ordered_branch_refs(snap: &RepoSnapshot) -> Vec<&RefInfo> {
    let mut out = Vec::with_capacity(snap.refs.len());
    out.extend(snap.refs.iter().filter(|r| r.kind == RefKind::LocalBranch));
    out.extend(snap.refs.iter().filter(|r| r.kind == RefKind::RemoteBranch));
    out.extend(snap.refs.iter().filter(|r| r.kind == RefKind::Tag));
    out
}

pub fn render_log_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let left_split =
        state
            .splitters
            .split(area, SplitterId::LogLeftHorizontal, SplitAxis::Horizontal);
    splitter_containers.push((
        SplitterId::LogLeftHorizontal,
        SplitAxis::Horizontal,
        left_split.container,
    ));

    let (branches_area, mid_right_area) = if left_split.collapsed {
        (Rect::ZERO, left_split.first)
    } else {
        (left_split.first, left_split.second)
    };

    if !left_split.collapsed {
        let branches_sub = state
            .repo_snapshot
            .as_ref()
            .map(|s| format!("{} refs", s.refs.len()))
            .unwrap_or_default();
        let branches_body = render_pane_frame(
            frame,
            branches_area,
            PaneId::LogBranchesTree,
            &branches_sub,
            ctx,
        );
        render_branches_pane(frame, branches_body, state, ctx);

        render_splitter_divider(
            frame,
            left_split.divider,
            SplitterId::LogLeftHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );
    }

    let right_split = state.splitters.split(
        mid_right_area,
        SplitterId::LogRightHorizontal,
        SplitAxis::Horizontal,
    );
    splitter_containers.push((
        SplitterId::LogRightHorizontal,
        SplitAxis::Horizontal,
        right_split.container,
    ));

    let commits_sub = state
        .repo_snapshot
        .as_ref()
        .map(|s| {
            if s.is_unborn_head {
                "unborn HEAD".to_string()
            } else {
                format!("{} commits", s.commits.len())
            }
        })
        .unwrap_or_default();
    let table_body = render_pane_frame(
        frame,
        right_split.first,
        PaneId::LogCommitTable,
        &commits_sub,
        ctx,
    );
    render_commits_table(frame, table_body, state, ctx);

    if !right_split.collapsed {
        render_splitter_divider(
            frame,
            right_split.divider,
            SplitterId::LogRightHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let details_body =
            render_pane_frame(frame, right_split.second, PaneId::LogDetailsDiff, "", ctx);
        render_commit_details(frame, details_body, state, ctx);
    }
}

fn render_branches_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(snap) = &state.repo_snapshot else {
        return;
    };

    let locals: Vec<_> = snap
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch)
        .collect();
    let remotes: Vec<_> = snap
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::RemoteBranch)
        .collect();
    let tags: Vec<_> = snap
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Tag)
        .collect();

    enum BranchRow<'a> {
        Header(String),
        Ref { node_idx: usize, info: &'a RefInfo },
    }

    let mut rows: Vec<BranchRow<'_>> = Vec::new();
    let mut node_idx = 0usize;

    let open = ctx.glyph_mode.symbols().tree_expanded;
    rows.push(BranchRow::Header(format!(
        " {open} Local ({})",
        locals.len()
    )));
    for r in &locals {
        rows.push(BranchRow::Ref { node_idx, info: r });
        node_idx += 1;
    }
    if !remotes.is_empty() {
        rows.push(BranchRow::Header(format!(
            " {open} Remote ({})",
            remotes.len()
        )));
        for r in &remotes {
            rows.push(BranchRow::Ref { node_idx, info: r });
            node_idx += 1;
        }
    }
    if !tags.is_empty() {
        rows.push(BranchRow::Header(format!(" {open} Tags ({})", tags.len())));
        for r in &tags {
            rows.push(BranchRow::Ref { node_idx, info: r });
            node_idx += 1;
        }
    }

    let focused = ctx.focused_pane == PaneId::LogBranchesTree;
    let selected_node = if node_idx == 0 {
        0
    } else {
        state.branches_selected_idx.min(node_idx - 1)
    };

    let visible_h = area.height as usize;
    for (vis_y, row) in rows.into_iter().take(visible_h).enumerate() {
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_y as u16), area.width, 1);
        match row {
            BranchRow::Header(title) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        truncate_to_width(&title, area.width as usize),
                        Style::default()
                            .fg(ctx.theme.fg_muted)
                            .add_modifier(Modifier::BOLD),
                    ))),
                    row_rect,
                );
            }
            BranchRow::Ref { node_idx, info } => {
                let is_sel = node_idx == selected_node;
                let is_hovered = matches!(
                    ctx.hover_target,
                    Some(HitTarget::BranchTreeNode { node_id, .. }) if node_id.0 as usize == node_idx
                );
                let bg =
                    row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);

                let glyphs = ctx.glyph_mode.symbols();
                let (icon, fg, text_fg) = match info.kind {
                    RefKind::LocalBranch if info.is_head => {
                        (glyphs.head, ctx.theme.ref_head.icon, ctx.theme.fg_primary)
                    }
                    RefKind::LocalBranch => (
                        glyphs.local_branch,
                        ctx.theme.ref_local.icon,
                        ctx.theme.fg_primary,
                    ),
                    RefKind::RemoteBranch => (
                        glyphs.remote_branch,
                        ctx.theme.ref_remote.icon,
                        ctx.theme.fg_primary,
                    ),
                    RefKind::Tag | RefKind::Head => {
                        (glyphs.tag, ctx.theme.ref_tag.icon, ctx.theme.fg_muted)
                    }
                };

                let ab = glyphs.format_ahead_behind(info.ahead_behind);
                let prefix = format!("  {icon} ");
                let max_name = (area.width as usize).saturating_sub(prefix.width());
                let line = Line::from(vec![
                    Span::styled(prefix, Style::default().fg(fg)),
                    Span::styled(
                        truncate_to_width(&format!("{}{}", info.name, ab), max_name),
                        Style::default().fg(if is_sel { ctx.theme.fg_bright } else { text_fg }),
                    ),
                ]);
                frame.render_widget(
                    Paragraph::new(line).style(Style::default().bg(bg)),
                    row_rect,
                );
                ctx.hit_registry.register(
                    row_rect,
                    2,
                    HitTarget::BranchTreeNode {
                        pane: PaneId::LogBranchesTree,
                        node_id: BranchTreeNodeId(node_idx as u32),
                    },
                );
            }
        }
    }
}

fn render_commits_table(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(snap) = &state.repo_snapshot else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Loading commit graph…",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    };

    let Some(cached) = &state.log_graph.cached else {
        return;
    };

    if snap.commits.is_empty() || cached.visible_count() == 0 {
        let msg = if snap.is_unborn_head {
            " Empty repository (unborn HEAD — no commits yet)"
        } else {
            " No commits found"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(ctx.theme.fg_muted))),
            area,
        );
        return;
    }

    let visible_h = area.height as usize;
    let total = cached.visible_count();
    let selected = state.log_graph.selected_idx.min(total.saturating_sub(1));
    let max_off = total.saturating_sub(visible_h);
    let mut offset = state.log_graph.scroll_offset.min(max_off);
    if selected < offset {
        offset = selected;
    } else if visible_h > 0 && selected >= offset + visible_h {
        offset = selected + 1 - visible_h;
    }

    let end = (offset + visible_h).min(total);
    let rows_len = end - offset;

    let focused = ctx.focused_pane == PaneId::LogCommitTable;
    let mut elem_gen =
        PrintElementGenerator::new(&cached.permanent, &cached.visible, snap.head_oid);

    struct PreparedLogRow {
        visible_row: VisibleRowIdx,
        commit_oid: Oid,
        row_bg: Color,
        elements: Vec<RenderGraphElement>,
    }

    let mut prepared_rows: Vec<PreparedLogRow> = Vec::with_capacity(rows_len);
    let mut max_track_pos = 0usize;

    for row_idx in offset..end {
        let vis_row = row_idx as VisibleRowIdx;
        let perm_id = cached.visible.linear.perm_id(vis_row);
        let commit = cached
            .commit_at_visible_row(snap, row_idx)
            .expect("visible row maps to valid commit");
        let is_sel = row_idx == selected;
        let is_in_current_branch = cached.current_branch_reachable.contains(perm_id as usize);
        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::CommitTableRow { visible_row, .. })
            | Some(HitTarget::GraphPrintElement { visible_row, .. })
            | Some(HitTarget::CommitRefPill { visible_row, .. })
            | Some(HitTarget::CommitRefOverflow { visible_row, .. })
                if *visible_row == row_idx
        );
        let default_bg = if is_in_current_branch {
            ctx.theme.bg_current_branch_row
        } else {
            ctx.theme.bg_canvas
        };
        let bg = row_background(is_sel, focused, is_hovered, default_bg, ctx.theme);

        let raw_elems = elem_gen.print_elements(vis_row);
        let mut rendered = Vec::with_capacity(raw_elems.len());
        for el in raw_elems {
            max_track_pos = max_track_pos.max(el.max_pos());
            let target_oid = match &el {
                GraphPrintElement::TerminalEdge { target_perm_id, .. } => {
                    Some(cached.permanent.linear.commit(*target_perm_id).oid)
                }
                _ => None,
            };
            let is_elem_selected = is_element_highlighted(&el, row_idx, ctx.hover_target);
            rendered.push(RenderGraphElement {
                element: el,
                target_oid,
                is_selected: is_elem_selected,
            });
        }
        prepared_rows.push(PreparedLogRow {
            visible_row: vis_row,
            commit_oid: commit.oid,
            row_bg: bg,
            elements: rendered,
        });
    }

    let max_graph_cols = ((area.width as usize * 45) / 100)
        .max(2)
        .min(area.width as usize);
    let graph_tracks = cached.recommended_tracks.max(max_track_pos + 1).max(1);
    let graph_cols = ((graph_tracks * 2).min(max_graph_cols) as u16).min(area.width);
    let graph_area = Rect::new(area.x, area.y, graph_cols, rows_len as u16);

    // Register row-level hit regions first (z=1) before graph elements & pills (z=2)
    for (vis_idx, row) in prepared_rows.iter().enumerate() {
        let row_idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        ctx.hit_registry.register(
            row_rect,
            1,
            HitTarget::CommitTableRow {
                visible_row: row_idx,
                commit_oid: row.commit_oid,
            },
        );
    }

    let row_slices: Vec<RowGraphSlice<'_>> = prepared_rows
        .iter()
        .map(|row| RowGraphSlice {
            visible_row: row.visible_row,
            commit_oid: row.commit_oid,
            row_bg: row.row_bg,
            elements: &row.elements,
        })
        .collect();

    let mut graph_backend = CellGridGraphBackend::new();
    let viewport_slice = GraphViewportSlice {
        rows: &row_slices,
        area: graph_area,
        occluding_rects: &[],
        color_map: &cached.color_map,
    };
    graph_backend.render_viewport(&viewport_slice, frame.buffer_mut(), ctx);

    for (vis_idx, row) in prepared_rows.iter().enumerate() {
        let bg = row.row_bg;
        let row_idx = offset + vis_idx;
        let commit = cached
            .commit_at_visible_row(snap, row_idx)
            .expect("visible row maps to valid commit");
        let row_y = area.y.saturating_add(vis_idx as u16);
        let text_x = area.x.saturating_add(graph_cols);
        let text_w = area.right().saturating_sub(text_x);
        if text_w == 0 {
            continue;
        }
        let text_rect = Rect::new(text_x, row_y, text_w, 1);
        let is_sel = row_idx == selected;
        let is_head = snap.head_oid == Some(commit.oid);

        let head_marker = is_head.then(|| RefInfo::head_marker(commit.oid));
        let groups = group_refs_for_table(
            snap.refs
                .iter()
                .filter(|r| r.target_oid == commit.oid)
                .chain(head_marker.as_ref()),
            GroupOptions {
                compact: state.compact_refs,
                show_tag_names: state.show_tag_names,
            },
        );

        let hash_prefix = Line::from(vec![Span::styled(
            format!(" {} ", commit.oid.short()),
            Style::default().fg(ctx.theme.fg_muted),
        )]);
        let hash_w = (hash_prefix.width() as u16).min(text_rect.width);
        frame.render_widget(
            Paragraph::new(hash_prefix).style(Style::default().bg(bg)),
            text_rect,
        );

        let pills_x = text_rect.x.saturating_add(hash_w);
        let pills_area = Rect::new(pills_x, row_y, text_rect.right().saturating_sub(pills_x), 1);
        let pills_w = PillBadgeBar {
            groups: &groups,
            glyphs: ctx.glyph_mode.symbols(),
            theme: ctx.theme,
            row_bg: bg,
            visible_row: row_idx,
        }
        .render(pills_area, frame.buffer_mut(), ctx.hit_registry);

        let subject_x = pills_x.saturating_add(pills_w);
        let subject_area = Rect::new(
            subject_x,
            row_y,
            text_rect.right().saturating_sub(subject_x),
            1,
        );
        let avail_subj = subject_area.width as usize;

        let subject = Span::styled(
            truncate_to_width(&commit.subject, avail_subj),
            Style::default()
                .fg(if is_sel || is_head {
                    ctx.theme.fg_bright
                } else if commit.parents.len() > 1 {
                    // MergeCommitsHighlighter
                    ctx.theme.fg_muted
                } else {
                    ctx.theme.fg_primary
                })
                .add_modifier(if is_head {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        );

        frame.render_widget(
            Paragraph::new(Line::from(subject)).style(Style::default().bg(bg)),
            subject_area,
        );
    }
}

fn is_element_highlighted(
    el: &GraphPrintElement,
    row_idx: usize,
    hover_target: Option<&HitTarget>,
) -> bool {
    let Some(HitTarget::GraphPrintElement {
        visible_row: hover_row,
        kind,
        ..
    }) = hover_target
    else {
        return false;
    };

    match (el, kind) {
        (GraphPrintElement::Node { .. }, GraphHitKind::Node { .. }) => *hover_row == row_idx,
        (
            GraphPrintElement::Edge {
                up_row, down_row, ..
            },
            GraphHitKind::Edge {
                up_row: h_up,
                down_row: h_dn,
            },
        )
        | (
            GraphPrintElement::Edge {
                up_row, down_row, ..
            },
            GraphHitKind::Arrow {
                up_row: h_up,
                down_row: h_dn,
                ..
            },
        )
        | (
            GraphPrintElement::TerminalEdge {
                up_row, down_row, ..
            },
            GraphHitKind::Edge {
                up_row: h_up,
                down_row: h_dn,
            },
        ) => (*up_row as usize, *down_row as usize) == (*h_up, *h_dn),
        (
            GraphPrintElement::TerminalEdge {
                up_row,
                down_row,
                target_row,
                direction,
                ..
            },
            GraphHitKind::Arrow {
                up_row: h_up,
                down_row: h_dn,
                target_row: h_target,
                is_down,
                ..
            },
        ) => {
            *hover_row == row_idx
                && (*up_row as usize, *down_row as usize) == (*h_up, *h_dn)
                && *target_row as usize == *h_target
                && (*direction == jgit_core::graph::EdgeDirection::Down) == *is_down
        }
        _ => false,
    }
}

fn render_commit_details(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(snap) = &state.repo_snapshot else {
        return;
    };
    let Some(base_commit) = state.log_commit_at(state.log_graph.selected_idx) else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " No commit selected",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    };
    let commit = state
        .commit_details
        .get(&base_commit.oid)
        .unwrap_or(base_commit);

    let containing = state
        .log_graph
        .cached
        .as_ref()
        .and_then(|c| {
            c.permanent.linear.node_of_oid(&commit.oid).map(|node| {
                ReachableNodes::containing_branches(&c.permanent.linear, node, &snap.refs)
            })
        })
        .unwrap_or_default();

    let parents_str = if commit.parents.is_empty() {
        "(root)".to_string()
    } else {
        commit
            .parents
            .iter()
            .map(|p| p.short())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" Commit: ", Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(
                commit.oid.to_string(),
                Style::default().fg(ctx.theme.fg_accent_link),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Author: ", Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(
                format!("{} <{}>", commit.author_name, commit.author_email),
                Style::default().fg(ctx.theme.fg_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Parents: ", Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(parents_str, Style::default().fg(ctx.theme.fg_primary)),
        ]),
    ];

    if !containing.is_empty() {
        let branch_names: Vec<&str> = containing.iter().take(6).map(|r| r.name.as_str()).collect();
        let suffix = if containing.len() > 6 {
            format!(" (+{})", containing.len() - 6)
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(" Branches: ", Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(
                format!("{}{suffix}", branch_names.join(", ")),
                Style::default().fg(ctx.theme.fg_primary),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", commit.subject),
        Style::default()
            .fg(ctx.theme.fg_bright)
            .add_modifier(Modifier::BOLD),
    )));

    frame.render_widget(Paragraph::new(lines), area);
}
