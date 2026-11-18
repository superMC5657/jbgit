pub mod banner;
pub mod commit;
pub mod commit_diff;
pub mod conflict_merge;
pub mod console;
pub mod dialogs;
pub mod header;
pub mod log;
pub mod log_branches;
pub mod log_details;
pub mod overlays;
pub mod palette_preview;
pub mod stash;

use jgit_core::{PaneId, SplitAxis, SplitterId, WorkspaceTab};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{AppState, RenderContext};
use crate::hit_test::HitTarget;
use crate::theme::ThemePalette;
use crate::widgets::OneCellSplitter;

pub fn render_app(frame: &mut Frame<'_>, state: &mut AppState) {
    let area = frame.area();
    state.terminal_area = area;
    state.hit_registry.clear();
    state.splitter_containers.clear();
    state.ensure_graph_cache();

    // Paint canvas background
    frame.render_widget(
        Block::default().style(Style::default().bg(state.theme.bg_canvas)),
        area,
    );

    if area.width < 40 || area.height < 10 {
        let msg = Paragraph::new(vec![
            Line::from(Span::styled(
                "Terminal too small",
                Style::default()
                    .fg(state.theme.fg_bright)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("Current: {}x{} (min 60x16)", area.width, area.height),
                Style::default().fg(state.theme.fg_muted),
            )),
        ])
        .alignment(Alignment::Center)
        .style(Style::default().bg(state.theme.bg_canvas));

        let y = area.y.saturating_add(area.height / 2).saturating_sub(1);
        let box_rect = Rect::new(area.x, y, area.width, area.height.min(2));
        frame.render_widget(msg, box_rect);
        return;
    }

    // Saturating layout: Row 0 = Global Header, Row 1 = Tool Tabs, Middle = Body, Last = Footer
    let header_h = 1u16.min(area.height);
    let tabs_h = 1u16.min(area.height.saturating_sub(header_h));
    let footer_h = 1u16.min(area.height.saturating_sub(header_h).saturating_sub(tabs_h));
    let banner_h = u16::from(banner::banner_visible(state))
        .min(area.height.saturating_sub(header_h + tabs_h + footer_h + 4));
    let body_h = area
        .height
        .saturating_sub(header_h)
        .saturating_sub(tabs_h)
        .saturating_sub(banner_h)
        .saturating_sub(footer_h);
    let banner_rect = Rect::new(
        area.x,
        area.y.saturating_add(header_h).saturating_add(tabs_h),
        area.width,
        banner_h,
    );

    let header_rect = Rect::new(area.x, area.y, area.width, header_h);
    let tabs_rect = Rect::new(area.x, area.y.saturating_add(header_h), area.width, tabs_h);
    let body_rect = Rect::new(
        area.x,
        area.y
            .saturating_add(header_h)
            .saturating_add(tabs_h)
            .saturating_add(banner_h),
        area.width,
        body_h,
    );
    let footer_rect = Rect::new(
        area.x,
        area.y
            .saturating_add(header_h)
            .saturating_add(tabs_h)
            .saturating_add(banner_h)
            .saturating_add(body_h),
        area.width,
        footer_h,
    );

    let theme = state.theme.clone();
    let branch_colors = state.branch_colors.clone();
    let glyph_mode = state.glyph_mode;
    let focused_pane = state.focused_pane;
    let hover_target = state.hover_target.clone();
    let dragging_splitter = match state.drag_state {
        Some(crate::app::DragState::Splitter { splitter_id, .. }) => Some(splitter_id),
        _ => None,
    };
    if state.active_tab == WorkspaceTab::Log {
        // Pane header + filter bar sit above the commit table.
        state.log_graph.last_viewport_h = body_h.saturating_sub(2) as usize;
    }

    // Register splitter containers separately after collecting them from layout helpers
    let mut splitter_containers = Vec::new();
    let mut hit_registry = std::mem::take(&mut state.hit_registry);

    {
        let mut ctx = RenderContext {
            theme: &theme,
            glyph_mode,
            branch_colors: &branch_colors,
            hit_registry: &mut hit_registry,
            focused_pane,
            hover_target: hover_target.as_ref(),
            dragging_splitter,
        };

        header::render_top_header(frame, header_rect, state, &mut ctx);
        header::render_tab_bar(frame, tabs_rect, state, &mut ctx);
        banner::render_banner(frame, banner_rect, state, &mut ctx);

        match state.active_tab {
            WorkspaceTab::Commit => {
                commit::render_commit_tab(
                    frame,
                    body_rect,
                    state,
                    &mut ctx,
                    &mut splitter_containers,
                );
            }
            WorkspaceTab::Log => {
                log::render_log_tab(frame, body_rect, state, &mut ctx, &mut splitter_containers);
            }
            WorkspaceTab::Stash => {
                stash::render_stash_tab(
                    frame,
                    body_rect,
                    state,
                    &mut ctx,
                    &mut splitter_containers,
                );
            }
            WorkspaceTab::Console => {
                console::render_console_tab(
                    frame,
                    body_rect,
                    state,
                    &mut ctx,
                    &mut splitter_containers,
                );
            }
        }

        header::render_footer_bar(frame, footer_rect, state, &mut ctx);
        overlays::render_hover_tooltip(frame, area, state, &ctx);
        overlays::render_modals(frame, area, state, &mut ctx);
    }

    state.hit_registry = hit_registry;
    for (id, axis, rect) in splitter_containers {
        state.splitter_containers.insert(id, (axis, rect));
    }
}

/// Renders a clean 1-line Darcula pane header bar (no outer box borders!) with an
/// active focus indicator bar (`▌` in `border_focused`) and registers `HitTarget::Pane`.
pub fn render_pane_frame(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: PaneId,
    subtitle: &str,
    ctx: &mut RenderContext<'_>,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::ZERO;
    }

    ctx.hit_registry.register(area, 1, HitTarget::Pane(pane));

    let is_focused = ctx.focused_pane == pane;
    let header_rect = Rect::new(area.x, area.y, area.width, 1);

    let bar_fg = if is_focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.fg_muted
    };
    let title_fg = if is_focused {
        ctx.theme.fg_bright
    } else {
        ctx.theme.fg_primary
    };
    let header_bg = if is_focused {
        ctx.theme.bg_panel
    } else {
        ctx.theme.bg_canvas
    };

    let mut spans = vec![
        Span::styled(
            if is_focused { "▌" } else { " " },
            Style::default().fg(bar_fg),
        ),
        Span::styled(
            pane.label(),
            Style::default().fg(title_fg).add_modifier(if is_focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
    ];
    if !subtitle.is_empty() {
        spans.push(Span::styled(
            format!("  {subtitle}"),
            Style::default().fg(ctx.theme.fg_muted),
        ));
    }

    let header = Paragraph::new(Line::from(spans)).style(Style::default().bg(header_bg));
    frame.render_widget(header, header_rect);

    Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    )
}

pub fn render_splitter_divider(
    frame: &mut Frame<'_>,
    divider: Rect,
    id: SplitterId,
    axis: SplitAxis,
    ctx: &mut RenderContext<'_>,
) {
    let active = ctx.dragging_splitter == Some(id)
        || matches!(
            ctx.hover_target,
            Some(HitTarget::Splitter { id: hovered_id, .. }) if *hovered_id == id
        );
    OneCellSplitter::render_divider(
        frame,
        divider,
        id,
        axis,
        active,
        ctx.theme,
        ctx.hit_registry,
    );
}

pub fn list_viewport(len: usize, selected: usize, visible_h: usize) -> (usize, usize) {
    if len == 0 || visible_h == 0 {
        return (0, 0);
    }
    let clamped = selected.min(len - 1);
    let offset = if clamped >= visible_h {
        clamped + 1 - visible_h
    } else {
        0
    };
    (clamped, offset)
}

pub fn row_background(
    is_sel: bool,
    is_focused: bool,
    is_hovered: bool,
    default_bg: Color,
    theme: &ThemePalette,
) -> Color {
    if is_sel {
        if is_focused {
            theme.bg_selection_focused
        } else {
            theme.bg_selection_unfocused
        }
    } else if is_hovered {
        theme.bg_hover
    } else {
        default_bg
    }
}

pub fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.width() <= max_width {
        return s.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let target = max_width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > target {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Renders `text` truncated to `max_width` columns with byte `ranges`
/// painted in `hl` (search hits, word-diff, issue refs).
pub fn highlight_spans(
    text: &str,
    ranges: &[(usize, usize)],
    max_width: usize,
    base: Style,
    hl: Style,
) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }
    let truncated = truncate_to_width(text, max_width);
    let (body, ellipsis) = match truncated.strip_suffix('…') {
        Some(b) if truncated.len() != text.len() || !text.ends_with('…') => (b, true),
        _ => (truncated.as_str(), false),
    };
    let limit = body.len();
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for &(s, e) in ranges {
        let (s, e) = (s.min(limit), e.min(limit));
        if s >= e || s < pos || !body.is_char_boundary(s) || !body.is_char_boundary(e) {
            continue;
        }
        if s > pos {
            spans.push(Span::styled(body[pos..s].to_string(), base));
        }
        spans.push(Span::styled(body[s..e].to_string(), hl));
        pos = e;
    }
    if pos < limit {
        spans.push(Span::styled(body[pos..].to_string(), base));
    }
    if ellipsis {
        spans.push(Span::styled("…", base));
    }
    spans
}

/// Greedy display-width-aware wrapping (breaks on spaces when possible).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    for word in text.split_inclusive(' ') {
        let w = word.width();
        if line_w + w > width && !line.is_empty() {
            out.push(line.trim_end().to_string());
            line.clear();
            line_w = 0;
        }
        if w > width {
            for ch in word.chars() {
                let cw = ch.width().unwrap_or(0);
                if line_w + cw > width && !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                line.push(ch);
                line_w += cw;
            }
        } else {
            line.push_str(word);
            line_w += w;
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line.trim_end().to_string());
    }
    out
}

/// Truncates `left` to make room for `right` (plus `right_pad` trailing spaces),
/// padding with spaces so `right` stays aligned to the right edge of `width`.
pub fn fit_left_right_spans(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
    right_pad: usize,
) -> Vec<Span<'static>> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let avail = width.saturating_sub(right_w + right_pad);
    let mut used = 0usize;
    let mut spans = Vec::new();
    for s in left {
        let w = s.content.width();
        if used + w <= avail {
            used += w;
            spans.push(s);
        } else {
            let rest = avail.saturating_sub(used);
            if rest > 0 {
                spans.push(Span::styled(truncate_to_width(&s.content, rest), s.style));
                used = avail;
            }
            break;
        }
    }
    spans.push(Span::raw(" ".repeat(avail.saturating_sub(used) + right_pad)));
    spans.extend(right);
    spans
}
