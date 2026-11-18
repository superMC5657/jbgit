use jgit_core::{
    BranchTreeNodeId, GroupOptions, PaneId, RefGroupKind, RefInfo, RefKind, RepoSnapshot,
    SplitAxis, SplitterId, group_refs_for_table,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;
use crate::views::{
    list_viewport, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
};
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

    if snap.commits.is_empty() {
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
    let (selected, offset) = list_viewport(snap.commits.len(), state.log_selected_idx, visible_h);

    let focused = ctx.focused_pane == PaneId::LogCommitTable;
    for (vis_idx, commit) in snap.commits.iter().skip(offset).take(visible_h).enumerate() {
        let row_idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        let is_sel = row_idx == selected;
        let is_head = snap.head_oid == Some(commit.oid);

        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::CommitTableRow { visible_row, .. }) if *visible_row == row_idx
        );
        let default_bg = if is_head {
            ctx.theme.bg_current_branch_row
        } else {
            ctx.theme.bg_canvas
        };
        let bg = row_background(is_sel, focused, is_hovered, default_bg, ctx.theme);

        // Refs at this commit, plus a synthetic HEAD marker on the HEAD commit
        // (merged into the current branch pill, or a detached-HEAD warning pill).
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

        let graph = &ctx.glyph_mode.symbols().graph;
        let dot = if is_head { graph.node_head } else { graph.node };
        let dot_color = groups
            .iter()
            .find(|g| g.kind != RefGroupKind::Tag && g.kind != RefGroupKind::DetachedHead)
            .map(|g| state.branch_colors.track_color_for_branch(g.primary_ref()))
            .unwrap_or(ctx.theme.track_palette[0].fg);

        let prefix = Line::from(vec![
            Span::styled(format!(" {dot} "), Style::default().fg(dot_color)),
            Span::styled(
                format!("{} ", commit.oid.short()),
                Style::default().fg(ctx.theme.fg_muted),
            ),
        ]);
        let prefix_w = (prefix.width() as u16).min(row_rect.width);
        frame.render_widget(
            Paragraph::new(prefix).style(Style::default().bg(bg)),
            row_rect,
        );

        ctx.hit_registry.register(
            row_rect,
            1,
            HitTarget::CommitTableRow {
                visible_row: row_idx,
                commit_oid: commit.oid,
            },
        );

        let pills_x = row_rect.x.saturating_add(prefix_w);
        let pills_area = Rect::new(
            pills_x,
            row_rect.y,
            row_rect.right().saturating_sub(pills_x),
            1,
        );
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
            row_rect.y,
            row_rect.right().saturating_sub(subject_x),
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
    let Some(base_commit) = snap.commits.get(state.log_selected_idx) else {
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

    let lines = vec![
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
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}", commit.subject),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}
