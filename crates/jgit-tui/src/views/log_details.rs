//! Right pane: `CommitChangesBrowser` (top), `CommitDetailsPanel` or inline
//! diff preview (bottom), and the full-screen zoomed diff.

use jgit_core::{
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, CommitDetails, DiffLine, DiffLineKind,
    FileDiff, MessageLineKind, PaneId, RefKind, SignatureStatus, classify_message_line,
    issue_ref_ranges, side_by_side_rows,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext};
use crate::hit_test::{ChangesToolbarButton, DiffToolbarButton, HitTarget};
use crate::log_view::{DiffPreviewDock, format_absolute_time};
use crate::syntax::{HighlightedDiff, render_diff_line_spans};
use crate::views::{
    fit_left_right_spans, highlight_spans, list_viewport, render_pane_frame, row_background,
    truncate_to_width, wrap_text,
};

fn muted_line(ctx: &RenderContext<'_>, text: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        text.to_string(),
        Style::default().fg(ctx.theme.fg_muted),
    ))
}

/// Renders a row of clickable toolbar buttons; returns the x after the last one.
fn render_toolbar<T: Copy>(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &mut RenderContext<'_>,
    buttons: &[(String, bool, T)],
    make_target: impl Fn(T) -> HitTarget,
) -> u16 {
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(ctx.theme.bg_panel)),
        area,
    );
    let mut x = area.x;
    for (text, active, id) in buttons {
        let w = text.width() as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let style = if *active {
            Style::default()
                .fg(ctx.theme.fg_bright)
                .bg(ctx.theme.bg_selection_unfocused)
        } else {
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(ctx.theme.bg_panel)
        };
        frame.render_widget(Paragraph::new(Span::styled(text.clone(), style)), rect);
        ctx.hit_registry.register(rect, 3, make_target(*id));
        x += w + 1;
    }
    x
}

// ───────────────────────────── Changes browser ─────────────────────────────

pub fn render_changes_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let details = &state.details;
    let bundle = details.current_bundle();
    let subtitle = match (bundle, &details.current_key) {
        (Some(b), _) => {
            let (add, del) = b.totals();
            let mut s = format!("{} files  +{add} -{del}", b.changes.len());
            if b.key.oids.len() > 1 {
                s = format!("{} commits · {s}", b.key.oids.len());
            }
            s
        }
        (None, Some(_)) => "Loading…".to_string(),
        (None, None) => String::new(),
    };
    let body = render_pane_frame(frame, area, PaneId::LogChangesTree, &subtitle, ctx);
    if body.height == 0 || body.width == 0 {
        return;
    }

    let merge_parents = bundle
        .filter(|b| b.key.oids.len() == 1)
        .and_then(|b| b.details.first())
        .map_or(0, |d| d.parents.len());
    let mut buttons = vec![
        (
            " ⊞ Dir ".to_string(),
            details.changes_group_by_dir,
            ChangesToolbarButton::GroupByDir,
        ),
        (" ⊕ ".to_string(), false, ChangesToolbarButton::ExpandAll),
        (" ⊖ ".to_string(), false, ChangesToolbarButton::CollapseAll),
        (
            " ◉ Diff ".to_string(),
            details.diff_preview,
            ChangesToolbarButton::DiffPreview,
        ),
    ];
    if merge_parents >= 2 {
        buttons.push((
            format!(" Parent {}/{merge_parents} ", details.merge_parent + 1),
            true,
            ChangesToolbarButton::SwitchParent,
        ));
    }
    render_toolbar(
        frame,
        Rect::new(body.x, body.y, body.width, 1),
        ctx,
        &buttons,
        HitTarget::ChangesToolbar,
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
            " Loading changes…"
        } else {
            " Select a commit to see its changes"
        };
        frame.render_widget(muted_line(ctx, msg), list);
        return;
    };
    if bundle.changes.is_empty() {
        frame.render_widget(muted_line(ctx, " No changes (empty commit)"), list);
        return;
    }
    let rows = details.change_rows();
    render_change_rows(frame, list, state, ctx, bundle, &rows);
}

fn render_change_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    bundle: &CommitChangesBundle,
    rows: &[ChangeTreeRow],
) {
    let focused = ctx.focused_pane == PaneId::LogChangesTree;
    let glyphs = ctx.glyph_mode.symbols();
    let (selected, offset) = list_viewport(
        rows.len(),
        state.details.changes_selected,
        area.height as usize,
    );
    for (vis_y, idx) in (offset..rows.len()).take(area.height as usize).enumerate() {
        let row = &rows[idx];
        let rect = Rect::new(area.x, area.y + vis_y as u16, area.width, 1);
        let hovered =
            matches!(ctx.hover_target, Some(HitTarget::LogChangeRow { idx: h }) if *h == idx);
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
                        "bin ",
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
        ctx.hit_registry
            .register(rect, 2, HitTarget::LogChangeRow { idx });
    }
}

// ───────────────────────────── Details / diff ─────────────────────────────

pub fn render_details_or_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if state.details.diff_preview && state.details.diff_dock == DiffPreviewDock::Details {
        render_docked_diff(frame, area, state, ctx);
    } else {
        let body = render_pane_frame(frame, area, PaneId::LogDetailsDiff, "", ctx);
        render_commit_details(frame, body, state, ctx);
    }
}

pub fn render_docked_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let subtitle = state
        .details
        .selected_change()
        .map(|c| format!("Diff · {}", c.path))
        .unwrap_or_else(|| "Diff Preview".to_string());
    let body = render_pane_frame(frame, area, PaneId::LogDetailsDiff, &subtitle, ctx);
    render_diff_view(frame, body, state, ctx, false);
}

pub fn render_zoomed_diff(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.height == 0 {
        return;
    }
    let files = state.details.file_row_positions();
    let pos = files
        .iter()
        .position(|&p| p == state.details.changes_selected)
        .map_or(0, |i| i + 1);
    let path = state
        .details
        .selected_change()
        .map(|c| c.path.clone())
        .unwrap_or_default();
    let header = format!(
        " ⤢ {path}   ({pos}/{})   [ / ] prev/next file · s split · c context · Esc back",
        files.len()
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_to_width(&header, area.width as usize),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ctx.theme.bg_panel)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    ctx.hit_registry
        .register(area, 1, HitTarget::Pane(PaneId::LogDetailsDiff));
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    render_diff_view(frame, body, state, ctx, true);
}

#[derive(Clone, Copy)]
enum DiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

fn diff_rows(diff: &FileDiff, side_by_side: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Header(h));
        if side_by_side {
            rows.extend(
                side_by_side_rows(hunk)
                    .into_iter()
                    .map(|r| DiffRow::Split(h, r.left, r.right)),
            );
        } else {
            rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Unified(h, l)));
        }
    }
    rows
}

fn render_diff_view(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
    zoomed: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let d = &state.details;
    let mut buttons = vec![
        (
            if d.diff_side_by_side {
                " ◫ Side-by-side "
            } else {
                " ▤ Unified "
            }
            .to_string(),
            d.diff_side_by_side,
            DiffToolbarButton::SideBySide,
        ),
        (
            format!(" ctx {} ", d.diff_context.label()),
            false,
            DiffToolbarButton::Context,
        ),
    ];
    if !zoomed {
        buttons.push((
            format!(" ◧ {} ", d.diff_dock.label()),
            d.diff_dock != DiffPreviewDock::Details,
            DiffToolbarButton::Dock,
        ));
    }
    buttons.push((
        if zoomed { " ⤡ Restore " } else { " ⤢ F4 " }.to_string(),
        zoomed,
        DiffToolbarButton::Zoom,
    ));
    buttons.push((" × ".to_string(), false, DiffToolbarButton::Close));
    render_toolbar(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        ctx,
        &buttons,
        HitTarget::DiffToolbar,
    );
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    if body.height == 0 {
        return;
    }
    ctx.hit_registry.register(body, 2, HitTarget::DiffArea);

    let Some(change) = d.selected_change() else {
        let msg = if d.current_bundle().is_none() {
            " Loading…"
        } else {
            " Select a file in Changes to preview its diff"
        };
        frame.render_widget(muted_line(ctx, msg), body);
        return;
    };
    let Some(diff) = d.current_diff() else {
        if change.is_binary {
            frame.render_widget(muted_line(ctx, " Binary file — no text diff"), body);
        } else {
            frame.render_widget(muted_line(ctx, " Loading diff…"), body);
        }
        return;
    };
    if diff.is_binary {
        frame.render_widget(muted_line(ctx, " Binary file — no text diff"), body);
        return;
    }
    if diff.hunks.is_empty() {
        frame.render_widget(
            muted_line(ctx, " No textual changes (mode or rename only)"),
            body,
        );
        return;
    }

    let rows = diff_rows(diff, d.diff_side_by_side);
    let mut total = rows.len();
    if diff.is_truncated {
        total += 1;
    }
    let h = body.height as usize;
    let scroll = d.diff_scroll.min(total.saturating_sub(h));
    let theme = ctx.theme;
    let num_w = 4usize;
    let computed_hl;
    let hl_diff: &HighlightedDiff = match d.current_diff_syntax() {
        Some(cached) => cached,
        None => {
            computed_hl = HighlightedDiff::build(diff);
            &computed_hl
        }
    };

    let line_style = |kind: DiffLineKind| -> (Style, Color, &'static str) {
        match kind {
            DiffLineKind::Added => (
                Style::default().fg(theme.fg_primary).bg(theme.diff_add_bg),
                theme.diff_add_word_bg,
                "+",
            ),
            DiffLineKind::Deleted => (
                Style::default().fg(theme.fg_primary).bg(theme.diff_del_bg),
                theme.diff_del_word_bg,
                "-",
            ),
            DiffLineKind::Context => (
                Style::default().fg(theme.fg_primary).bg(theme.bg_canvas),
                theme.bg_canvas,
                " ",
            ),
        }
    };
    let gutter = Style::default().fg(theme.fg_muted).bg(theme.bg_panel);
    let num = |n: Option<u32>| n.map_or_else(|| " ".repeat(num_w), |v| format!("{v:>num_w$}"));

    let side_cell = |hh: usize,
                     line_idx: Option<usize>,
                     lineno: fn(&DiffLine) -> Option<u32>,
                     width: usize|
     -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        match line_idx {
            Some(li) => {
                let l = &diff.hunks[hh].lines[li];
                let toks = hl_diff
                    .hunks
                    .get(hh)
                    .and_then(|h| h.get(li))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(l.kind);
                spans.push(Span::styled(format!("{} ", num(lineno(l))), gutter));
                spans.push(Span::styled(marker.to_string(), base));
                let text_w = width.saturating_sub(num_w + 2);
                let mut body = render_diff_line_spans(
                    &l.text,
                    toks,
                    &l.word_ranges,
                    text_w,
                    base,
                    word_bg,
                    theme.true_color,
                );
                let used: usize = body.iter().map(|s| s.content.width()).sum();
                body.push(Span::styled(" ".repeat(text_w.saturating_sub(used)), base));
                spans.extend(body);
            }
            None => {
                spans.push(Span::styled(" ".repeat(num_w + 1), gutter));
                spans.push(Span::styled(
                    " ".repeat(width.saturating_sub(num_w + 1)),
                    Style::default().bg(theme.bg_panel),
                ));
            }
        }
        spans
    };

    let width = body.width as usize;
    for (vis_y, row_idx) in (scroll..total).take(h).enumerate() {
        let rect = Rect::new(body.x, body.y + vis_y as u16, body.width, 1);
        let line = match rows.get(row_idx) {
            None => Line::from(Span::styled(
                " … diff truncated (too large)",
                Style::default().fg(theme.status_conflicted),
            )),
            Some(DiffRow::Header(hh)) => Line::from(Span::styled(
                truncate_to_width(&diff.hunks[*hh].header, width),
                Style::default()
                    .fg(theme.fg_accent_link)
                    .bg(theme.diff_mod_bg),
            ))
            .style(Style::default().bg(theme.diff_mod_bg)),
            Some(DiffRow::Unified(hh, l)) => {
                let dl = &diff.hunks[*hh].lines[*l];
                let toks = hl_diff
                    .hunks
                    .get(*hh)
                    .and_then(|h| h.get(*l))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (base, word_bg, marker) = line_style(dl.kind);
                let mut spans = vec![
                    Span::styled(
                        format!("{} {} ", num(dl.old_lineno), num(dl.new_lineno)),
                        gutter,
                    ),
                    Span::styled(marker.to_string(), base),
                ];
                let text_w = width.saturating_sub(num_w * 2 + 3);
                spans.extend(render_diff_line_spans(
                    &dl.text,
                    toks,
                    &dl.word_ranges,
                    text_w,
                    base,
                    word_bg,
                    theme.true_color,
                ));
                Line::from(spans).style(base)
            }
            Some(DiffRow::Split(hh, left, right)) => {
                let half = width / 2;
                let mut spans = side_cell(*hh, *left, |l| l.old_lineno, half);
                spans.push(Span::styled(
                    "│",
                    Style::default().fg(theme.border_splitter),
                ));
                spans.extend(side_cell(*hh, *right, |l| l.new_lineno, width - half - 1));
                Line::from(spans)
            }
        };
        frame.render_widget(Paragraph::new(line), rect);
    }
}

fn person_line(label: &str, p: &jgit_core::PersonStamp, ctx: &RenderContext<'_>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label:<10}"),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(p.name.clone(), Style::default().fg(ctx.theme.fg_bright)),
        Span::styled(
            format!(" <{}>", p.email),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(
            format!("  {}", format_absolute_time(p.time, true)),
            Style::default().fg(ctx.theme.fg_primary),
        ),
    ])
}

/// Hit target attached to a details line at an x-offset range.
struct LineHit {
    line_idx: usize,
    x: u16,
    width: u16,
    target: HitTarget,
}

fn render_commit_details(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry.register(area, 1, HitTarget::DetailsArea);
    let details = &state.details;
    let Some(key) = &details.current_key else {
        frame.render_widget(muted_line(ctx, " No commit selected"), area);
        return;
    };
    let Some(bundle) = details.current_bundle() else {
        // Instant fallback from the snapshot while details load.
        if let [oid] = key.oids.as_slice()
            && let Some(c) = state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.commits.iter().find(|c| c.oid == *oid))
        {
            let lines = vec![
                Line::from(vec![
                    Span::styled(" Commit    ", Style::default().fg(ctx.theme.fg_muted)),
                    Span::styled(
                        c.oid.to_string(),
                        Style::default().fg(ctx.theme.fg_accent_link),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    format!(" {}", c.subject),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    " Loading details…",
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), area);
        } else {
            frame.render_widget(muted_line(ctx, " Loading details…"), area);
        }
        return;
    };

    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineHit> = Vec::new();

    if bundle.details.len() > 1 {
        lines.push(Line::from(Span::styled(
            format!(" {} commits selected", bundle.details.len()),
            Style::default()
                .fg(ctx.theme.fg_bright)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        for d in &bundle.details {
            let short = d.oid.short();
            let subject = d.message.lines().next().unwrap_or("").to_string();
            hits.push(LineHit {
                line_idx: lines.len(),
                x: 1,
                width: short.width() as u16,
                target: HitTarget::DetailsParent { oid: d.oid },
            });
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {short}"),
                    Style::default().fg(ctx.theme.fg_accent_link),
                ),
                Span::styled(
                    format!(" {}", truncate_to_width(&subject, width.saturating_sub(10))),
                    Style::default().fg(ctx.theme.fg_primary),
                ),
            ]));
        }
    } else if let Some(d) = bundle.details.first() {
        build_single_details(d, state, ctx, width, &mut lines, &mut hits);
    }

    let h = area.height as usize;
    let scroll = details.details_scroll.min(lines.len().saturating_sub(h));
    for (vis_y, idx) in (scroll..lines.len()).take(h).enumerate() {
        let rect = Rect::new(area.x, area.y + vis_y as u16, area.width, 1);
        frame.render_widget(Paragraph::new(lines[idx].clone()), rect);
        for hit in &hits {
            if hit.line_idx == idx && hit.x < area.width {
                let w = hit.width.min(area.width - hit.x);
                ctx.hit_registry
                    .register(Rect::new(area.x + hit.x, rect.y, w, 1), 3, hit.target.clone());
            }
        }
    }
}

fn build_single_details(
    d: &CommitDetails,
    state: &AppState,
    ctx: &RenderContext<'_>,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineHit>,
) {
    let theme = ctx.theme;
    let label = |s: &str| Span::styled(format!(" {s:<10}"), Style::default().fg(theme.fg_muted));

    // 1. Full SHA (click to copy) + signature badge.
    let sha = d.oid.to_string();
    let mut sha_spans = vec![
        label("Commit"),
        Span::styled(
            sha.clone(),
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ];
    match d.signature {
        SignatureStatus::Verified => sha_spans.push(Span::styled(
            " ✓ Verified ",
            Style::default()
                .fg(theme.ref_local.pill_fg)
                .bg(theme.ref_local.pill_bg),
        )),
        SignatureStatus::SignedUnverified => sha_spans.push(Span::styled(
            " ⚠ Unverified ",
            Style::default()
                .fg(theme.ref_detached.pill_fg)
                .bg(theme.ref_detached.pill_bg),
        )),
        SignatureStatus::Unsigned => {}
    }
    hits.push(LineHit {
        line_idx: lines.len(),
        x: 11,
        width: sha.width() as u16,
        target: HitTarget::DetailsHash { oid: d.oid },
    });
    lines.push(Line::from(sha_spans));

    // 2. Author / Committer.
    if d.same_author_and_committer() {
        lines.push(person_line("Author", &d.author, ctx));
    } else {
        lines.push(person_line("Author", &d.author, ctx));
        lines.push(person_line("Committer", &d.committer, ctx));
    }

    // 3. Clickable parents.
    let mut parent_spans = vec![label("Parents")];
    if d.parents.is_empty() {
        parent_spans.push(Span::styled(
            "(root commit)",
            Style::default().fg(theme.fg_muted),
        ));
    }
    let mut x = 11u16;
    for p in &d.parents {
        let short = p.short();
        hits.push(LineHit {
            line_idx: lines.len(),
            x,
            width: short.width() as u16,
            target: HitTarget::DetailsParent { oid: *p },
        });
        x += short.width() as u16 + 2;
        parent_spans.push(Span::styled(
            short,
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ));
        parent_spans.push(Span::raw("  "));
    }
    lines.push(Line::from(parent_spans));

    // 4. Contained in branches / tags.
    if let (Some((oid, names)), Some(snap)) = (&state.details.containing, &state.repo_snapshot)
        && *oid == d.oid
    {
        let kind_of = |n: &str| snap.refs.iter().find(|r| r.name == n).map(|r| r.kind);
        let branches: Vec<&str> = names
            .iter()
            .filter(|n| kind_of(n) != Some(RefKind::Tag))
            .map(String::as_str)
            .collect();
        let tags: Vec<&str> = names
            .iter()
            .filter(|n| kind_of(n) == Some(RefKind::Tag))
            .map(String::as_str)
            .collect();
        let mut push_list = |title: &str, items: &[&str], color: Color| {
            if items.is_empty() {
                return;
            }
            let joined = items.join(", ");
            for (i, chunk) in wrap_text(&joined, width.saturating_sub(12).max(8))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    if i == 0 {
                        label(title)
                    } else {
                        Span::raw(" ".repeat(11))
                    },
                    Span::styled(chunk, Style::default().fg(color)),
                ]));
            }
        };
        push_list("Branches", &branches, theme.ref_local.pill_fg);
        push_list("Tags", &tags, theme.ref_tag.pill_fg);
        if branches.is_empty() && tags.is_empty() {
            lines.push(Line::from(vec![
                label("Branches"),
                Span::styled("(not on any branch)", Style::default().fg(theme.fg_muted)),
            ]));
        }
    }

    // 5. Full message.
    lines.push(Line::from(""));
    for (idx, raw) in d.message.lines().enumerate() {
        let kind = classify_message_line(idx, raw);
        let base = match kind {
            MessageLineKind::Subject => Style::default()
                .fg(theme.fg_bright)
                .add_modifier(Modifier::BOLD),
            MessageLineKind::Trailer => Style::default().fg(theme.ref_remote.pill_fg),
            MessageLineKind::Body => Style::default().fg(theme.fg_primary),
        };
        let issue = Style::default()
            .fg(theme.fg_accent_link)
            .add_modifier(Modifier::UNDERLINED);
        if raw.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        for chunk in wrap_text(raw, width.saturating_sub(2).max(8)) {
            let ranges = issue_ref_ranges(&chunk);
            let mut spans = vec![Span::raw(" ")];
            spans.extend(highlight_spans(
                &chunk,
                &ranges,
                width.saturating_sub(1),
                base,
                issue,
            ));
            lines.push(Line::from(spans));
        }
    }
}
