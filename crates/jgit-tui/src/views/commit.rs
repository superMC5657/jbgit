use jgit_core::{ChangeNodeId, FileStatusKind, PaneId, SplitAxis, SplitterId, TreeId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;
use crate::views::{
    list_viewport, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
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

    let left_split = state.splitters.split(
        main_split.first,
        SplitterId::CommitLeftVertical,
        SplitAxis::Vertical,
    );
    splitter_containers.push((
        SplitterId::CommitLeftVertical,
        SplitAxis::Vertical,
        left_split.container,
    ));

    // Left-top: Changes tree (live StageStatusSnapshot)
    let total_changes = state.stage_status.as_ref().map_or(0, |s| s.total_changes());
    let changes_sub = format!("{total_changes} files");
    let changes_body = render_pane_frame(
        frame,
        left_split.first,
        PaneId::CommitChangesTree,
        &changes_sub,
        ctx,
    );
    render_stage_list(frame, changes_body, state, ctx);

    if !left_split.collapsed {
        render_splitter_divider(
            frame,
            left_split.divider,
            SplitterId::CommitLeftVertical,
            SplitAxis::Vertical,
            ctx,
        );

        let msg_body = render_pane_frame(
            frame,
            left_split.second,
            PaneId::CommitMessageEditor,
            "Spec 04",
            ctx,
        );
        let msg = Paragraph::new(Line::from(Span::styled(
            " Commit message editor & Amend (Spec 04)",
            Style::default().fg(ctx.theme.fg_muted),
        )));
        frame.render_widget(msg, msg_body);
    }

    if !main_split.collapsed {
        render_splitter_divider(
            frame,
            main_split.divider,
            SplitterId::CommitMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let diff_body = render_pane_frame(
            frame,
            main_split.second,
            PaneId::CommitDiffPreview,
            "Spec 04",
            ctx,
        );
        let hint = Paragraph::new(Line::from(Span::styled(
            " Three-doc line tracker & partial hunk diff (Spec 04)",
            Style::default().fg(ctx.theme.fg_muted),
        )));
        frame.render_widget(hint, diff_body);
    }
}

fn render_stage_list(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(status) = &state.stage_status else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Loading stage status…",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    };

    let mut rows: Vec<(&str, FileStatusKind, &str)> = Vec::new();
    for e in &status.conflicted {
        rows.push(("Conflict", e.kind, &e.path));
    }
    for e in &status.staged {
        rows.push(("Staged", e.kind, &e.path));
    }
    for e in &status.unstaged {
        rows.push(("Unstaged", e.kind, &e.path));
    }
    for e in &status.untracked {
        rows.push(("Untracked", e.kind, &e.path));
    }

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " Working tree clean",
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    }

    let visible_h = area.height as usize;
    let (selected, offset) = list_viewport(rows.len(), state.commit_stage_selected_idx, visible_h);

    let focused = ctx.focused_pane == PaneId::CommitChangesTree;
    for (vis_idx, (group, kind, path)) in rows.iter().skip(offset).take(visible_h).enumerate() {
        let row_idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        let is_sel = row_idx == selected;

        let fg_kind = ctx.theme.status_color(*kind);

        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::ChangesTreeNode { node_id, .. }) if node_id.0 as usize == row_idx
        );
        let bg = row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);

        let file_icon = ctx.glyph_mode.symbols().file_icon(path);
        let max_path = (area.width as usize).saturating_sub(17);
        let line = Line::from(vec![
            Span::styled(
                format!(" [{}] ", kind.badge_char()),
                Style::default().fg(fg_kind),
            ),
            Span::styled(
                format!("{group:<9} "),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(format!("{file_icon} "), Style::default().fg(fg_kind)),
            Span::styled(
                truncate_to_width(path, max_path),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry.register(
            row_rect,
            1,
            HitTarget::ChangesTreeNode {
                tree_id: TreeId(0),
                node_id: ChangeNodeId(row_idx as u32),
                on_hover_action_icon: false,
            },
        );
    }
}
