use jbgit_core::{
    CommitDetails, MessageLineKind, PersonStamp, RefKind, SignatureStatus, classify_message_line,
    issue_ref_ranges,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, RenderContext, ScrollPane};
use crate::hit_test::HitTarget;
use crate::log_view::format_absolute_time;
use crate::views::{highlight_spans, truncate_to_width, wrap_text};

use super::{muted_line, pad_label};

fn person_line(label: &str, p: &PersonStamp, ctx: &RenderContext<'_>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            pad_label(label, 10),
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

pub(super) fn render_commit_details(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    ctx.hit_registry.register(area, 1, HitTarget::DetailsArea);
    let h = area.height as usize;
    let details = &state.log.details;
    let Some(key) = &details.changes.current_key else {
        ctx.record_scroll_bound(ScrollPane::LogDetails, h, 0);
        frame.render_widget(
            muted_line(ctx, ctx.t("view.log_details.no_commit_selected")),
            area,
        );
        return;
    };
    let Some(bundle) = details.current_bundle(&state.diff_caches) else {
        if let [oid] = key.oids.as_slice()
            && let Some(c) = state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.commits.iter().find(|c| c.oid == *oid))
        {
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        pad_label(ctx.t("view.log_details.commit_label"), 10),
                        Style::default().fg(ctx.theme.fg_muted),
                    ),
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
                    ctx.t("view.log_details.loading_details"),
                    Style::default().fg(ctx.theme.fg_muted),
                )),
            ];
            let fallback_max = lines.len().saturating_sub(h);
            frame.render_widget(Paragraph::new(lines), area);
            ctx.record_scroll_bound(ScrollPane::LogDetails, h, fallback_max);
        } else {
            ctx.record_scroll_bound(ScrollPane::LogDetails, h, 0);
            frame.render_widget(
                muted_line(ctx, ctx.t("view.log_details.loading_details")),
                area,
            );
        }
        return;
    };

    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineHit> = Vec::new();

    if bundle.details.len() > 1 {
        let count_s = bundle.details.len().to_string();
        lines.push(Line::from(Span::styled(
            ctx.t_fmt("view.log_details.commits_selected", &[("count", &count_s)]),
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

    let max_scroll = lines.len().saturating_sub(h);
    ctx.record_scroll_bound(ScrollPane::LogDetails, h, max_scroll);
    let scroll = details.details_scroll.offset().min(max_scroll);
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

pub(crate) fn signature_badge_and_desc_keys(
    status: SignatureStatus,
) -> (&'static str, &'static str) {
    match status {
        SignatureStatus::Unsigned => ("", ""),
        SignatureStatus::Pending => (
            "view.log_details.sig_badge.pending",
            "view.log_details.sig_desc.pending",
        ),
        SignatureStatus::Good => (
            "view.log_details.sig_badge.good",
            "view.log_details.sig_desc.good",
        ),
        SignatureStatus::GoodUnknownValidity => (
            "view.log_details.sig_badge.unknown_validity",
            "view.log_details.sig_desc.unknown_validity",
        ),
        SignatureStatus::Expired { key_expired: false } => (
            "view.log_details.sig_badge.expired_sig",
            "view.log_details.sig_desc.expired_sig",
        ),
        SignatureStatus::Expired { key_expired: true } => (
            "view.log_details.sig_badge.expired_key",
            "view.log_details.sig_desc.expired_key",
        ),
        SignatureStatus::Revoked => (
            "view.log_details.sig_badge.revoked",
            "view.log_details.sig_desc.revoked",
        ),
        SignatureStatus::CannotCheck => (
            "view.log_details.sig_badge.cannot_check",
            "view.log_details.sig_desc.cannot_check",
        ),
        SignatureStatus::Bad => (
            "view.log_details.sig_badge.bad",
            "view.log_details.sig_desc.bad",
        ),
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
    let label = |s: &str| Span::styled(pad_label(s, 10), Style::default().fg(theme.fg_muted));

    let sha = d.oid.to_string();
    let mut sha_spans = vec![
        label(ctx.t("view.log_details.commit_label")),
        Span::styled(
            sha.clone(),
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ];
    let effective_sig = state
        .diff_caches
        .signatures
        .peek(&d.oid)
        .unwrap_or(&d.signature);
    if effective_sig.status.is_signed() {
        let (badge_key, _) = signature_badge_and_desc_keys(effective_sig.status);
        let (_, pill_fg, pill_bg) = theme.signature_colors(effective_sig.status);
        sha_spans.push(Span::raw(" "));
        sha_spans.push(Span::styled(
            ctx.t(badge_key).to_string(),
            Style::default().fg(pill_fg).bg(pill_bg),
        ));
    }
    hits.push(LineHit {
        line_idx: lines.len(),
        x: 11,
        width: sha.width() as u16,
        target: HitTarget::DetailsHash { oid: d.oid },
    });
    lines.push(Line::from(sha_spans));

    if d.same_author_and_committer() {
        lines.push(person_line(ctx.t("view.log_details.author_label"), &d.author, ctx));
    } else {
        lines.push(person_line(ctx.t("view.log_details.author_label"), &d.author, ctx));
        lines.push(person_line(
            ctx.t("view.log_details.committer_label"),
            &d.committer,
            ctx,
        ));
    }

    let mut parent_spans = vec![label(ctx.t("view.log_details.parents_label"))];
    if d.parents.is_empty() {
        parent_spans.push(Span::styled(
            ctx.t("view.log_details.root_commit").to_string(),
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

    if effective_sig.status.is_signed() {
        let (_, desc_key) = signature_badge_and_desc_keys(effective_sig.status);
        let (icon_fg, _, _) = theme.signature_colors(effective_sig.status);
        let full_desc = format!("{} {}", effective_sig.status.icon(), ctx.t(desc_key));
        for (i, chunk) in wrap_text(&full_desc, width.saturating_sub(12).max(12))
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                if i == 0 {
                    label(ctx.t("view.log_details.signature_label"))
                } else {
                    Span::raw(" ".repeat(11))
                },
                Span::styled(chunk, Style::default().fg(icon_fg)),
            ]));
        }
        if let Some(signer) = &effective_sig.signer {
            lines.push(Line::from(vec![
                label(ctx.t("view.log_details.sig_signer_label")),
                Span::styled(signer.clone(), Style::default().fg(theme.fg_bright)),
            ]));
        }
        if let Some(key) = &effective_sig.key {
            lines.push(Line::from(vec![
                label(ctx.t("view.log_details.sig_key_label")),
                Span::styled(key.clone(), Style::default().fg(theme.fg_primary)),
            ]));
        }
    }

    if let (Some((oid, names)), Some(snap)) = (&state.log.details.containing, &state.repo_snapshot)
        && *oid == d.oid
    {
        let kind_of = |n: &str| {
            state
                .log
                .graph
                .cached
                .as_ref()
                .and_then(|c| c.ref_kind_by_name.get(n).copied())
                .or_else(|| snap.refs.iter().find(|r| r.name == n).map(|r| r.kind))
        };
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
        push_list(
            ctx.t("view.log_details.branches_label"),
            &branches,
            theme.ref_local.pill_fg,
        );
        push_list(
            ctx.t("view.log_details.tags_label"),
            &tags,
            theme.ref_tag.pill_fg,
        );
        if branches.is_empty() && tags.is_empty() {
            lines.push(Line::from(vec![
                label(ctx.t("view.log_details.branches_label")),
                Span::styled(
                    ctx.t("view.log_details.not_on_branch").to_string(),
                    Style::default().fg(theme.fg_muted),
                ),
            ]));
        }
    }

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
