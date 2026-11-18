use jgit_core::{PaneId, SplitAxis, SplitterId};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{AppState, RenderContext};
use crate::views::{render_pane_frame, render_splitter_divider};

pub fn render_stash_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    splitter_containers: &mut Vec<(SplitterId, SplitAxis, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let split = state
        .splitters
        .split(area, SplitterId::StashMainHorizontal, SplitAxis::Horizontal);
    splitter_containers.push((
        SplitterId::StashMainHorizontal,
        SplitAxis::Horizontal,
        split.container,
    ));

    let list_body = render_pane_frame(frame, split.first, PaneId::StashList, "", ctx);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Stash stack (Spec 05)",
            Style::default().fg(ctx.theme.fg_muted),
        ))),
        list_body,
    );

    if !split.collapsed {
        render_splitter_divider(
            frame,
            split.divider,
            SplitterId::StashMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let diff_body = render_pane_frame(frame, split.second, PaneId::StashDiffPreview, "", ctx);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " Stash diff preview (Spec 05)",
                Style::default().fg(ctx.theme.fg_muted),
            ))),
            diff_body,
        );
    }
}
