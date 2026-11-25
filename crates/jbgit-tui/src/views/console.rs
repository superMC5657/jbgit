use jbgit_core::{
    ConsoleLineSeverity, GitConsoleEntry, PaneId, SplitAxis, SplitterId, classify_console_line,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::HitTarget;
use crate::theme::ThemePalette;
use crate::views::{
    list_viewport, render_pane_frame, render_splitter_divider, row_background, truncate_to_width,
};

/// Total rendered lines in the console output detail pane for `entry`
/// (command header + separator rule + output lines or placeholder line).
pub fn console_entry_total_lines(entry: &GitConsoleEntry) -> usize {
    entry.output_lines.len().max(1) + 2
}

struct ConsoleScrollView {
    total_lines: usize,
    max_scroll: usize,
    scroll: usize,
}

fn console_scroll_view(
    entry: &GitConsoleEntry,
    visible_h: usize,
    current_scroll: usize,
) -> ConsoleScrollView {
    let total_lines = console_entry_total_lines(entry);
    let max_scroll = total_lines.saturating_sub(visible_h);
    let scroll = current_scroll.min(max_scroll);
    ConsoleScrollView {
        total_lines,
        max_scroll,
        scroll,
    }
}

fn severity_style(severity: ConsoleLineSeverity, theme: &ThemePalette) -> Style {
    let fg = match severity {
        ConsoleLineSeverity::Normal | ConsoleLineSeverity::Info => theme.fg_primary,
        ConsoleLineSeverity::Warning => theme.ref_detached.icon,
        ConsoleLineSeverity::Error => theme.status_conflicted,
    };
    Style::default().fg(fg)
}

pub fn render_console_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let split = state.splitters.split(
        area,
        SplitterId::ConsoleMainHorizontal,
        SplitAxis::Horizontal,
    );
    ctx.record_splitter_container(
        SplitterId::ConsoleMainHorizontal,
        SplitAxis::Horizontal,
        split.container,
    );

    let count_s = state.console.len().to_string();
    let list_sub = state
        .i18n
        .t_fmt("view.console.list_subtitle", &[("count", &count_s)]);
    let list_body = render_pane_frame(
        frame,
        split.first,
        PaneId::ConsoleCommandList,
        &list_sub,
        ctx,
    );
    render_command_list(frame, list_body, state, ctx);

    if !split.collapsed {
        render_splitter_divider(
            frame,
            split.divider,
            SplitterId::ConsoleMainHorizontal,
            SplitAxis::Horizontal,
            ctx,
        );

        let detail_sub = state
            .console
            .selected_entry()
            .map(|e| {
                let base = format!("#{} {}", e.id, e.timestamp_label());
                let visible_h = split.second.height.saturating_sub(1) as usize;
                let sv = console_scroll_view(e, visible_h, state.console.output_scroll());
                if visible_h > 0 && sv.max_scroll > 0 {
                    let start = sv.scroll + 1;
                    let end = (sv.scroll + visible_h).min(sv.total_lines);
                    let pct = (sv.scroll * 100 + sv.max_scroll / 2) / sv.max_scroll;
                    format!("{base}  L{start}-{end}/{} ({pct}%)", sv.total_lines)
                } else {
                    base
                }
            })
            .unwrap_or_default();
        let out_body = render_pane_frame(
            frame,
            split.second,
            PaneId::ConsoleOutputDetail,
            &detail_sub,
            ctx,
        );
        render_command_output(frame, out_body, state, ctx);
    }
}

fn render_command_list(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if state.console.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                state.i18n.t("view.console.empty_list"),
                Style::default().fg(ctx.theme.fg_muted),
            )),
            area,
        );
        return;
    }

    let visible_h = area.height as usize;
    let (selected, offset) =
        list_viewport(state.console.len(), state.console.selected_idx(), visible_h);

    let focused = ctx.focused_pane == PaneId::ConsoleCommandList;

    for (vis_idx, entry) in state
        .console
        .entries()
        .iter()
        .skip(offset)
        .take(visible_h)
        .enumerate()
    {
        let idx = offset + vis_idx;
        let row_rect = Rect::new(area.x, area.y.saturating_add(vis_idx as u16), area.width, 1);
        let is_sel = idx == selected;

        let is_hovered = matches!(
            ctx.hover_target,
            Some(HitTarget::ConsoleCommandRow { idx: h }) if *h == idx
        );
        let bg = row_background(is_sel, focused, is_hovered, ctx.theme.bg_canvas, ctx.theme);

        let (badge_txt, badge_fg) = match entry.exit_code {
            None => (
                format!(" {} ", state.spinner_char()),
                ctx.theme.border_focused,
            ),
            Some(0) => (" ✓ ".to_string(), ctx.theme.status_added),
            Some(code) => (format!(" ✗{code} "), ctx.theme.status_conflicted),
        };

        let dur = entry
            .duration_ms
            .map(|ms| format!(" {ms}ms"))
            .unwrap_or_else(|| " …".to_string());

        let ts = format!(" [{}]", entry.timestamp_label());
        let reserved = ts.width() + badge_txt.width() + dur.width();
        let max_cmd = (area.width as usize).saturating_sub(reserved);

        let line = Line::from(vec![
            Span::styled(ts, Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(
                badge_txt,
                Style::default().fg(badge_fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_to_width(&entry.command_line, max_cmd),
                Style::default().fg(if is_sel {
                    ctx.theme.fg_bright
                } else {
                    ctx.theme.fg_primary
                }),
            ),
            Span::styled(dur, Style::default().fg(ctx.theme.fg_muted)),
        ]);

        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(bg)),
            row_rect,
        );
        ctx.hit_registry
            .register(row_rect, 1, HitTarget::ConsoleCommandRow { idx });
    }
}

fn render_command_output(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    ctx.hit_registry
        .register(area, 1, HitTarget::ConsoleOutputArea);

    let visible_h = area.height as usize;
    let Some(entry) = state.console.selected_entry() else {
        ctx.record_scroll_bound(ScrollPane::ConsoleOutput, visible_h, 0);
        let mut lines = vec![Line::from(Span::styled(
            state.i18n.t("view.console.select_prompt"),
            Style::default().fg(ctx.theme.fg_muted),
        ))];
        if let Some((summary, is_err)) = state.git_executable_summary() {
            lines.push(Line::from(""));
            let style = if is_err {
                Style::default()
                    .fg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ctx.theme.fg_primary)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    " {}",
                    truncate_to_width(&summary, area.width.saturating_sub(1) as usize)
                ),
                style,
            )));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };

    let mut lines: Vec<Line<'_>> = Vec::new();
    let status_span = match entry.exit_code {
        None => {
            let sp = state.spinner_char().to_string();
            Span::styled(
                state
                    .i18n
                    .t_fmt("view.console.running", &[("spinner", &sp)]),
                Style::default().fg(ctx.theme.border_focused),
            )
        }
        Some(0) => {
            let ms = entry.duration_ms.unwrap_or_default().to_string();
            Span::styled(
                state
                    .i18n
                    .t_fmt("view.console.exit_status", &[("code", "0"), ("ms", &ms)]),
                Style::default()
                    .fg(ctx.theme.status_added)
                    .add_modifier(Modifier::BOLD),
            )
        }
        Some(code) => {
            let code_s = code.to_string();
            let ms = entry.duration_ms.unwrap_or_default().to_string();
            Span::styled(
                state.i18n.t_fmt(
                    "view.console.exit_status",
                    &[("code", &code_s), ("ms", &ms)],
                ),
                Style::default()
                    .fg(ctx.theme.status_conflicted)
                    .add_modifier(Modifier::BOLD),
            )
        }
    };

    lines.push(Line::from(vec![
        Span::styled(
            format!(" $ {}  ", entry.command_line),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ),
        status_span,
    ]));
    lines.push(Line::from(Span::styled(
        " ────────────────────────────────────────",
        Style::default().fg(ctx.theme.border_splitter),
    )));

    if !entry.has_output() {
        lines.push(Line::from(Span::styled(
            state.i18n.t("view.console.no_output"),
            Style::default().fg(ctx.theme.fg_muted),
        )));
    } else {
        for line in &entry.output_lines {
            let severity = classify_console_line(line, entry.exit_code);
            lines.push(Line::from(Span::styled(
                format!(" {}", line.text),
                severity_style(severity, ctx.theme),
            )));
        }
    }

    let sv = console_scroll_view(entry, visible_h, state.console.output_scroll());
    debug_assert_eq!(sv.total_lines, lines.len());
    ctx.record_scroll_bound(ScrollPane::ConsoleOutput, visible_h, sv.max_scroll);
    let total_lines = sv.total_lines;
    let max_scroll = sv.max_scroll;
    let scroll = sv.scroll;

    let show_scrollbar = max_scroll > 0 && area.width >= 4;
    let content_area = if show_scrollbar {
        Rect::new(area.x, area.y, area.width - 1, area.height)
    } else {
        area
    };

    let slice: Vec<Line<'_>> = lines.into_iter().skip(scroll).take(visible_h).collect();
    frame.render_widget(Paragraph::new(slice), content_area);

    if show_scrollbar {
        let thumb_h = ((visible_h * visible_h) / total_lines).clamp(1, visible_h);
        let travel = visible_h - thumb_h;
        let thumb_top = if travel == 0 {
            0
        } else {
            ((scroll * travel + max_scroll / 2) / max_scroll).min(travel)
        };
        let thumb_end = thumb_top + thumb_h;
        let track_style = Style::default().fg(ctx.theme.border_splitter);
        let thumb_style = Style::default().fg(ctx.theme.fg_muted);
        let sb_x = area.right() - 1;
        let buf = frame.buffer_mut();
        for row in 0..visible_h {
            let y = area.y.saturating_add(row as u16);
            let is_thumb = row >= thumb_top && row < thumb_end;
            let cell = &mut buf[(sb_x, y)];
            cell.set_symbol(if is_thumb { "┃" } else { "│" });
            cell.set_style(if is_thumb { thumb_style } else { track_style });
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Local;
    use jbgit_core::{ConsoleLine, ConsoleStream, GitConsoleEntry};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::config::AppConfig;
    use crate::hit_test::HitTestRegistry;
    use crate::theme::GlyphMode;

    fn make_state(entry: GitConsoleEntry) -> AppState {
        let mut state = AppState::new_with_config(
            std::path::PathBuf::from("."),
            None,
            AppConfig::load_defaults(),
        );
        state.console.push_started(entry);
        state
    }

    fn render_with_theme(
        theme: &ThemePalette,
        w: u16,
        h: u16,
        draw_fn: impl FnOnce(&mut Frame<'_>, Rect, &mut RenderContext<'_>),
    ) -> Buffer {
        let branch_colors = theme.branch_color_manager();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitTestRegistry::new();
        terminal
            .draw(|f| {
                let mut ctx =
                    RenderContext::minimal(theme, GlyphMode::Unicode, &branch_colors, &mut hits);
                draw_fn(f, f.area(), &mut ctx);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_output_with_theme(state: &AppState, theme: &ThemePalette, w: u16, h: u16) -> Buffer {
        render_with_theme(theme, w, h, |f, area, ctx| {
            render_console_tab(f, area, state, ctx);
        })
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn find_row_containing(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buf.area.height {
            let line = row_text(buf, y);
            if let Some(byte_idx) = line.find(needle) {
                let col = line[..byte_idx].chars().count() as u16;
                return Some((y, col));
            }
        }
        None
    }

    #[test]
    fn successful_git_pull_renders_chronologically_without_stderr_prefix() {
        let mut entry = GitConsoleEntry {
            id: 1,
            started_at: Local::now(),
            duration_ms: Some(420),
            command_line: "git pull".into(),
            output_lines: Vec::new(),
            exit_code: Some(0),
        };
        entry.push_line(
            ConsoleStream::Stderr,
            "From https://github.com/sxyazi/yazi".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "   40a91421..d5e4beb6  main       -> origin/main".into(),
        );
        entry.push_line(
            ConsoleStream::Stdout,
            " create mode 100644 yazi-widgets/src/cmp/mod.rs".into(),
        );

        let state = make_state(entry);
        let theme = ThemePalette::darcula(true);
        let buf = render_output_with_theme(&state, &theme, 120, 20);

        let full_text: Vec<String> = (0..buf.area.height).map(|y| row_text(&buf, y)).collect();
        let joined = full_text.join("\n");
        assert!(
            !joined.contains("[stderr]"),
            "unexpected [stderr] in:\n{joined}"
        );

        let (from_y, from_x) = find_row_containing(&buf, "From https://github.com/sxyazi/yazi")
            .expect("missing fetch From line");
        let (range_y, range_x) =
            find_row_containing(&buf, "40a91421..d5e4beb6").expect("missing fetch ref range line");
        let (create_y, create_x) = find_row_containing(&buf, "create mode 100644")
            .expect("missing merge create mode line");

        assert!(
            from_y < range_y && range_y < create_y,
            "expected chronological order (from={from_y}, range={range_y}, create={create_y})"
        );

        assert_eq!(buf[(from_x, from_y)].fg, theme.fg_primary);
        assert_eq!(buf[(range_x, range_y)].fg, theme.fg_primary);
        assert_eq!(buf[(create_x, create_y)].fg, theme.fg_primary);
    }

    #[test]
    fn warnings_and_errors_use_semantic_colors_across_builtin_themes() {
        let mut entry = GitConsoleEntry {
            id: 2,
            started_at: Local::now(),
            duration_ms: Some(150),
            command_line: "git pull".into(),
            output_lines: Vec::new(),
            exit_code: Some(128),
        };
        entry.push_line(
            ConsoleStream::Stderr,
            "warning: pulling without specifying how to reconcile divergent branches".into(),
        );
        entry.push_line(
            ConsoleStream::Stderr,
            "fatal: Not possible to fast-forward, aborting.".into(),
        );

        let state = make_state(entry);
        for theme in [
            ThemePalette::darcula(true),
            ThemePalette::catppuccin_mocha(true),
            ThemePalette::gruvbox_dark(true),
        ] {
            let buf = render_output_with_theme(&state, &theme, 140, 20);
            let (warn_y, warn_x) =
                find_row_containing(&buf, "warning: pulling").expect("missing warning line");
            let (fatal_y, fatal_x) =
                find_row_containing(&buf, "fatal: Not possible").expect("missing fatal line");

            assert_eq!(buf[(warn_x, warn_y)].fg, theme.ref_detached.icon);
            assert_eq!(buf[(fatal_x, fatal_y)].fg, theme.status_conflicted);
            assert_ne!(theme.ref_detached.icon, theme.fg_primary);
            assert_ne!(theme.status_conflicted, theme.fg_primary);
        }
    }

    #[test]
    fn long_output_scroll_clamps_and_slices_output_lines() {
        let entry = GitConsoleEntry {
            id: 3,
            started_at: Local::now(),
            duration_ms: Some(25),
            command_line: "git log".into(),
            output_lines: (0..30)
                .map(|i| ConsoleLine {
                    stream: if i % 2 == 0 {
                        ConsoleStream::Stdout
                    } else {
                        ConsoleStream::Stderr
                    },
                    text: format!("entry-{i:02}"),
                })
                .collect(),
            exit_code: Some(0),
        };
        let mut state = make_state(entry);
        state.console.set_output_scroll_for_test(999);

        let theme = ThemePalette::darcula(true);
        let buf = render_with_theme(&theme, 80, 10, |f, area, ctx| {
            render_command_output(f, area, &state, ctx);
        });
        let last_row = row_text(&buf, 9);
        assert!(
            last_row.contains("entry-29"),
            "expected last row to clamp to entry-29, got: {last_row}"
        );
        assert!(
            find_row_containing(&buf, "entry-00").is_none(),
            "expected early lines to be scrolled out of viewport"
        );
    }

    #[test]
    fn short_output_hides_scroll_indicator_and_scrollbar() {
        let entry = GitConsoleEntry {
            id: 4,
            started_at: Local::now(),
            duration_ms: Some(10),
            command_line: "git status".into(),
            output_lines: (0..4)
                .map(|i| ConsoleLine {
                    stream: ConsoleStream::Stdout,
                    text: format!("short-{i}"),
                })
                .collect(),
            exit_code: Some(0),
        };
        let state = make_state(entry);
        let theme = ThemePalette::darcula(true);
        let buf = render_output_with_theme(&state, &theme, 120, 20);
        let header_row = row_text(&buf, 0);
        assert!(
            !header_row.contains('%'),
            "short output should not show percentage: {header_row}"
        );
        for y in 1..buf.area.height {
            let sym = buf[(buf.area.width - 1, y)].symbol();
            assert_ne!(sym, "┃", "unexpected scrollbar thumb at row {y}");
            assert_ne!(sym, "│", "unexpected scrollbar track at row {y}");
        }
    }

    #[test]
    fn long_output_renders_percentage_and_scrollbar_thumb() {
        let entry = GitConsoleEntry {
            id: 5,
            started_at: Local::now(),
            duration_ms: Some(30),
            command_line: "git pull".into(),
            output_lines: (0..60)
                .map(|i| ConsoleLine {
                    stream: ConsoleStream::Stdout,
                    text: format!("line-{i:02}"),
                })
                .collect(),
            exit_code: Some(0),
        };
        let mut state = make_state(entry);
        let theme = ThemePalette::darcula(true);

        // At top -> 0% and thumb starts at top of output body (row 1).
        let buf_top = render_output_with_theme(&state, &theme, 120, 20);
        let header_top = row_text(&buf_top, 0);
        assert!(
            header_top.contains("(0%)"),
            "expected (0%) in header: {header_top}"
        );
        assert_eq!(buf_top[(119, 1)].symbol(), "┃");

        // At bottom -> 100% and thumb reaches bottom row (row 19).
        state.console.set_output_scroll_for_test(usize::MAX);
        let buf_bot = render_output_with_theme(&state, &theme, 120, 20);
        let header_bot = row_text(&buf_bot, 0);
        assert!(
            header_bot.contains("(100%)"),
            "expected (100%) in header: {header_bot}"
        );
        assert_eq!(buf_bot[(119, 19)].symbol(), "┃");
    }
}
