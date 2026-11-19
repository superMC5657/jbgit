//! Built-in palette preview (F9): 14 branch tracks with WCAG ratios, the
//! semantic tokens, sample ref pills on normal / selected rows, and a glyph
//! ruler that makes any double-width rendering visible.

use jbgit_core::{BranchColorId, GroupOptions, Oid, RefInfo, RefKind, group_refs_for_table};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::RenderContext;
use crate::hit_test::HitTestRegistry;
use crate::theme::color::contrast_ratio;
use crate::theme::{CANVAS_RGB, DARCULA_TRACKS, TRACK_COUNT, TRACK_NAMES, ThemePalette};
use crate::widgets::PillBadgeBar;

const TRACK_CELL_W: u16 = 46;
const TOKEN_CELL_W: u16 = 26;

pub fn render_palette_preview(frame: &mut Frame<'_>, area: Rect, ctx: &RenderContext<'_>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = ctx.theme;
    let mut y = area.y;
    let bottom = area.bottom();
    let line = |frame: &mut Frame<'_>, y: &mut u16, l: Line<'_>| {
        if *y < bottom {
            frame.render_widget(Paragraph::new(l), Rect::new(area.x, *y, area.width, 1));
            *y += 1;
        }
    };
    let heading = |s: &str| {
        Line::from(Span::styled(
            format!(" {s}"),
            Style::default()
                .fg(theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let muted = Style::default().fg(theme.fg_muted);

    line(
        frame,
        &mut y,
        Line::from(vec![
            Span::styled(
                format!(" Glyphs: {}", ctx.glyph_mode.label()),
                Style::default().fg(theme.fg_primary),
            ),
            Span::styled(
                format!(
                    "   Color: {}",
                    if theme.true_color {
                        "TrueColor 24-bit"
                    } else {
                        "ANSI-256 (CIE76)"
                    }
                ),
                Style::default().fg(theme.fg_primary),
            ),
            Span::styled("   F12 toggle glyphs · Esc close", muted),
        ]),
    );
    line(frame, &mut y, Line::default());

    // --- 14 branch tracks -------------------------------------------------
    line(
        frame,
        &mut y,
        heading("Branch tracks — fg on canvas · muted container · WCAG vs #1E1F22"),
    );
    let graph = &ctx.glyph_mode.symbols().graph;
    let colors = theme.branch_color_manager();
    let cols = (area.width / TRACK_CELL_W).clamp(1, 2) as usize;
    let rows = DARCULA_TRACKS.len().div_ceil(cols);
    for r in 0..rows {
        let mut spans = Vec::new();
        for c in 0..cols {
            let slot = c * rows + r;
            let Some(&(fg_rgb, _)) = DARCULA_TRACKS.get(slot) else {
                continue;
            };
            let track = colors.track(slot);
            let (rr, gg, bb) = fg_rgb;
            spans.push(Span::styled(
                format!(" {slot:>2} {:<8}", TRACK_NAMES[slot]),
                muted,
            ));
            spans.push(Span::styled(
                format!("{h}{h}{h}{} ", graph.node, h = graph.horizontal),
                Style::default().fg(track.fg),
            ));
            spans.push(Span::styled(
                format!(" {:<7} ", TRACK_NAMES[slot]),
                Style::default().fg(track.fg).bg(track.muted_bg),
            ));
            spans.push(Span::styled(
                format!(
                    " #{rr:02X}{gg:02X}{bb:02X} {:>5.2}:1  ",
                    contrast_ratio(fg_rgb, CANVAS_RGB)
                ),
                Style::default().fg(theme.fg_primary),
            ));
        }
        line(frame, &mut y, Line::from(spans));
    }

    // 20 colliding ids (all hash to slot 0): 14 shifted palette slots, then HSB.
    let ids: Vec<BranchColorId> = (0..20)
        .map(|k| BranchColorId(k * TRACK_COUNT as i32))
        .collect();
    let mut spans = vec![Span::styled(" >14 concurrent tracks  ", muted)];
    for color in colors.assign_visible(&ids) {
        spans.push(Span::styled(
            format!("{} ", graph.node),
            Style::default().fg(color),
        ));
    }
    spans.push(Span::styled(
        " (collisions shift slots; HSB fallback beyond 14)",
        muted,
    ));
    line(frame, &mut y, Line::from(spans));
    line(frame, &mut y, Line::default());

    // --- Semantic tokens ----------------------------------------------------
    line(frame, &mut y, heading("Semantic tokens"));
    let tokens = semantic_tokens(theme);
    let per_row = (area.width / TOKEN_CELL_W).max(1) as usize;
    for chunk in tokens.chunks(per_row) {
        let spans: Vec<Span<'_>> = chunk
            .iter()
            .flat_map(|(name, color)| {
                [
                    Span::raw(" "),
                    Span::styled("  ", Style::default().bg(*color)),
                    Span::styled(
                        format!(" {:<width$}", name, width = TOKEN_CELL_W as usize - 4),
                        Style::default().fg(theme.fg_primary),
                    ),
                ]
            })
            .collect();
        line(frame, &mut y, Line::from(spans));
    }
    line(frame, &mut y, Line::default());

    // --- Ref pills ------------------------------------------------------------
    line(
        frame,
        &mut y,
        heading("Ref pills — normal row / focused selection / detached HEAD"),
    );
    let attached = sample_groups(false);
    let detached = sample_groups(true);
    let mut scratch_hits = HitTestRegistry::new();
    for (groups, row_bg) in [
        (&attached, theme.bg_canvas),
        (&attached, theme.bg_selection_focused),
        (&detached, theme.bg_current_branch_row),
    ] {
        if y >= bottom {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        frame.render_widget(Paragraph::new(" ").style(Style::default().bg(row_bg)), row);
        let inner = Rect::new(row.x + 1, row.y, row.width.saturating_sub(1), 1);
        let used = PillBadgeBar {
            groups,
            glyphs: ctx.glyph_mode.symbols(),
            theme,
            row_bg,
            visible_row: 0,
        }
        .render(inner, frame.buffer_mut(), &mut scratch_hits);
        let subj_x = inner.x.saturating_add(used);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "feat: commit subject",
                Style::default().fg(theme.fg_bright),
            ))
            .style(Style::default().bg(row_bg)),
            Rect::new(subj_x, y, row.right().saturating_sub(subj_x), 1),
        );
        y += 1;
    }
    line(frame, &mut y, Line::default());

    // --- Glyph ruler -----------------------------------------------------------
    line(
        frame,
        &mut y,
        heading("Glyph ruler — every glyph must sit exactly between two │ markers"),
    );
    let mut spans = vec![Span::styled(" │", muted)];
    for (_, glyph) in ctx.glyph_mode.symbols().named() {
        spans.push(Span::styled(glyph, Style::default().fg(theme.fg_bright)));
        spans.push(Span::styled("│", muted));
    }
    line(frame, &mut y, Line::from(spans));
}

fn semantic_tokens(t: &ThemePalette) -> Vec<(&'static str, Color)> {
    vec![
        ("bg_canvas", t.bg_canvas),
        ("bg_panel", t.bg_panel),
        ("bg_elevated", t.bg_elevated),
        ("bg_selection_focused", t.bg_selection_focused),
        ("bg_selection_unfocused", t.bg_selection_unfocused),
        ("bg_current_branch_row", t.bg_current_branch_row),
        ("bg_hover", t.bg_hover),
        ("border_splitter", t.border_splitter),
        ("border_focused", t.border_focused),
        ("fg_primary", t.fg_primary),
        ("fg_bright", t.fg_bright),
        ("fg_muted", t.fg_muted),
        ("fg_accent_link", t.fg_accent_link),
        ("status_modified", t.status_modified),
        ("status_added", t.status_added),
        ("status_untracked", t.status_untracked),
        ("status_deleted", t.status_deleted),
        ("status_renamed", t.status_renamed),
        ("status_conflicted", t.status_conflicted),
        ("diff_add_bg", t.diff_add_bg),
        ("diff_add_word_bg", t.diff_add_word_bg),
        ("diff_del_bg", t.diff_del_bg),
        ("diff_del_word_bg", t.diff_del_word_bg),
        ("diff_mod_bg", t.diff_mod_bg),
        ("ref_head", t.ref_head.icon),
        ("ref_detached", t.ref_detached.icon),
        ("ref_local", t.ref_local.icon),
        ("ref_remote", t.ref_remote.icon),
        ("ref_tag", t.ref_tag.icon),
        ("ref_tracked pill", t.ref_tracked.pill_bg),
    ]
}

fn sample_groups(detached: bool) -> Vec<jbgit_core::RefGroup> {
    let mk = |name: &str, kind: RefKind, upstream: Option<&str>, is_head: bool| RefInfo {
        name: name.into(),
        full_name: name.into(),
        target_oid: Oid::ZERO,
        kind,
        upstream: upstream.map(Into::into),
        ahead_behind: None,
        is_head,
    };
    let mut refs = vec![
        mk("main", RefKind::LocalBranch, Some("origin/main"), !detached),
        mk("origin/main", RefKind::RemoteBranch, None, false),
        mk("feat/登录优化", RefKind::LocalBranch, None, false),
        mk("origin/feat/dag", RefKind::RemoteBranch, None, false),
        mk("v0.1.0", RefKind::Tag, None, false),
    ];
    refs.push(RefInfo::head_marker(Oid::ZERO));
    group_refs_for_table(&refs, GroupOptions::default())
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::theme::GlyphMode;

    fn render(mode: GlyphMode) -> ratatui::buffer::Buffer {
        let theme = ThemePalette::darcula(true);
        let branch_colors = theme.branch_color_manager();
        let mut terminal = Terminal::new(TestBackend::new(110, 40)).unwrap();
        let mut hits = HitTestRegistry::new();
        terminal
            .draw(|f| {
                let ctx = RenderContext::minimal(&theme, mode, &branch_colors, &mut hits);
                render_palette_preview(f, f.area(), &ctx);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn line_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn preview_snapshot_contains_all_sections_in_both_modes() {
        for mode in [GlyphMode::Unicode, GlyphMode::NerdFont] {
            let buf = render(mode);
            let all: Vec<String> = (0..buf.area.height).map(|y| line_text(&buf, y)).collect();
            let joined = all.join("\n");
            assert!(joined.contains(mode.label()));
            assert!(joined.contains("Rose") && joined.contains("Slate"));
            assert!(joined.contains("#8176F6"), "{joined}");
            assert!(joined.contains("diff_del_word_bg"));
            assert!(joined.contains(">14 concurrent tracks"));
            let caps = mode.symbols();
            assert!(
                joined.contains(&format!(
                    "{} origin & main{}",
                    caps.head, caps.pill_cap_right
                )),
                "{joined}"
            );

            // Glyph ruler: markers at every other column ⇒ each glyph is 1 column.
            let ruler = all
                .iter()
                .rev()
                .find(|l| l.trim_start().starts_with('│'))
                .unwrap();
            let glyph_count = mode.symbols().named().len();
            let ruler = ruler.trim_end();
            assert_eq!(ruler.width(), 2 + glyph_count * 2, "{ruler}");
            for (i, ch) in ruler.chars().skip(1).enumerate() {
                if i % 2 == 0 {
                    assert_eq!(ch, '│', "marker misaligned at {i}: {ruler}");
                }
            }
        }
    }
}
