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

fn person_line(
    label: &str,
    p: &PersonStamp,
    width: usize,
    ctx: &RenderContext<'_>,
) -> Line<'static> {
    let label_str = pad_label(label, 10);
    let label_w = label_str.width();
    let time_str = format!("  {}", format_absolute_time(p.time, true));
    let time_w = time_str.width();
    let email_str = format!(" <{}>", p.email);
    let name_w = p.name.width();
    let email_w = email_str.width();

    let mut spans = vec![Span::styled(
        label_str,
        Style::default().fg(ctx.theme.fg_muted),
    )];

    // Elide email (then author name) before the timestamp so full `YYYY-MM-DD HH:MM`
    // stays intact on narrow Details panes (e.g. 140x40).
    let person_budget = width.saturating_sub(label_w + time_w);
    if name_w + email_w <= person_budget {
        spans.push(Span::styled(
            p.name.clone(),
            Style::default().fg(ctx.theme.fg_bright),
        ));
        spans.push(Span::styled(
            email_str,
            Style::default().fg(ctx.theme.fg_muted),
        ));
    } else if name_w <= person_budget {
        spans.push(Span::styled(
            p.name.clone(),
            Style::default().fg(ctx.theme.fg_bright),
        ));
        let rem = person_budget.saturating_sub(name_w);
        if rem >= 6 {
            let inner_email = truncate_to_width(&p.email, rem.saturating_sub(3));
            spans.push(Span::styled(
                format!(" <{inner_email}>"),
                Style::default().fg(ctx.theme.fg_muted),
            ));
        }
    } else if person_budget >= 3 {
        spans.push(Span::styled(
            truncate_to_width(&p.name, person_budget),
            Style::default().fg(ctx.theme.fg_bright),
        ));
    } else {
        spans.push(Span::styled(
            truncate_to_width(&p.name, width.saturating_sub(label_w)),
            Style::default().fg(ctx.theme.fg_bright),
        ));
        return Line::from(spans);
    }

    spans.push(Span::styled(
        time_str,
        Style::default().fg(ctx.theme.fg_primary),
    ));
    Line::from(spans)
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
        if let Some(blocked) = &details.many_changes_blocked {
            let count_s = blocked.oids.len().to_string();
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    ctx.t_fmt("view.log_details.commits_selected", &[("count", &count_s)]),
                    Style::default()
                        .fg(ctx.theme.fg_bright)
                        .add_modifier(Modifier::BOLD),
                ))),
                area,
            );
        } else {
            frame.render_widget(
                muted_line(ctx, ctx.t("view.log_details.no_commit_selected")),
                area,
            );
        }
        return;
    };
    let Some(bundle) = details.current_bundle(&state.diff_caches) else {
        if let [oid] = key.oids.as_slice()
            && let Some(c) = state
                .repo_snapshot
                .as_ref()
                .and_then(|s| s.commits.iter().find(|c| c.oid == *oid))
        {
            let full_sha = c.oid.to_string();
            let shown_sha = if 11 + full_sha.width() <= area.width as usize {
                full_sha
            } else {
                c.oid.short()
            };
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        pad_label(ctx.t("view.log_details.commit_label"), 10),
                        Style::default().fg(ctx.theme.fg_muted),
                    ),
                    Span::styled(shown_sha, Style::default().fg(ctx.theme.fg_accent_link)),
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
                ctx.hit_registry.register(
                    Rect::new(area.x + hit.x, rect.y, w, 1),
                    3,
                    hit.target.clone(),
                );
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

    let full_sha = d.oid.to_string();
    let max_btn = format!(" [{}] ", ctx.t("view.log_details.maximize_msg_btn"));
    let max_btn_w = max_btn.width() as u16;
    let effective_sig = state
        .diff_caches
        .signatures
        .peek(&d.oid)
        .unwrap_or(&d.signature);
    let sig_badge = if effective_sig.status.is_signed() {
        let (badge_key, _) = signature_badge_and_desc_keys(effective_sig.status);
        let (_, pill_fg, pill_bg) = theme.signature_colors(effective_sig.status);
        Some((ctx.t(badge_key).to_string(), pill_fg, pill_bg))
    } else {
        None
    };
    let sig_extra_w = sig_badge
        .as_ref()
        .map_or(0usize, |(txt, _, _)| 1 + txt.width());
    // Fall back to short SHA when the 40-char hash + signature badge + `[⤢ Full]` button
    // would overflow a narrow Details pane (e.g. 140x40).
    let sha = if 11 + full_sha.width() + sig_extra_w + 1 + max_btn_w as usize <= width {
        full_sha
    } else {
        d.oid.short()
    };
    let mut sha_spans = vec![
        label(ctx.t("view.log_details.commit_label")),
        Span::styled(
            sha.clone(),
            Style::default()
                .fg(theme.fg_accent_link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ];
    let mut sha_used_w: u16 = 11 + sha.width() as u16;
    if let Some((badge_txt, pill_fg, pill_bg)) = sig_badge {
        sha_used_w = sha_used_w.saturating_add(1 + badge_txt.width() as u16);
        sha_spans.push(Span::raw(" "));
        sha_spans.push(Span::styled(
            badge_txt,
            Style::default().fg(pill_fg).bg(pill_bg),
        ));
    }
    hits.push(LineHit {
        line_idx: lines.len(),
        x: 11,
        width: sha.width() as u16,
        target: HitTarget::DetailsHash { oid: d.oid },
    });
    if width >= (sha_used_w + max_btn_w + 1) as usize {
        sha_spans.push(Span::raw(" "));
        hits.push(LineHit {
            line_idx: lines.len(),
            x: sha_used_w + 1,
            width: max_btn_w,
            target: HitTarget::DetailsMaximizeMessage,
        });
        sha_spans.push(Span::styled(
            max_btn,
            Style::default().fg(theme.fg_bright).bg(theme.bg_panel),
        ));
    }
    lines.push(Line::from(sha_spans));

    if d.same_author_and_committer() {
        lines.push(person_line(
            ctx.t("view.log_details.author_label"),
            &d.author,
            width,
            ctx,
        ));
    } else {
        lines.push(person_line(
            ctx.t("view.log_details.author_label"),
            &d.author,
            width,
            ctx,
        ));
        lines.push(person_line(
            ctx.t("view.log_details.committer_label"),
            &d.committer,
            width,
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
    let snap = state.repo_snapshot.as_deref();
    for (idx, raw) in d.message.lines().enumerate() {
        let kind = classify_message_line(idx, raw);
        let base = match kind {
            MessageLineKind::Subject => Style::default()
                .fg(theme.fg_bright)
                .add_modifier(Modifier::BOLD),
            MessageLineKind::Trailer => Style::default().fg(theme.ref_remote.pill_fg),
            MessageLineKind::Body => Style::default().fg(theme.fg_primary),
        };
        if raw.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        for chunk in wrap_text(raw, width.saturating_sub(2).max(8)) {
            let line_idx = lines.len();
            let (spans, hash_hits) =
                render_body_chunk_with_links(&chunk, width.saturating_sub(1), base, snap, theme);
            for (hx, hw, target_oid) in hash_hits {
                hits.push(LineHit {
                    line_idx,
                    x: hx,
                    width: hw,
                    target: HitTarget::DetailsCommitHash { oid: target_oid },
                });
            }
            lines.push(Line::from(spans));
        }
    }
}

/// Finds word-bounded 7..=40 hex commit hash tokens in `text` that resolve to a
/// commit in `snap`, returning `(byte_start, byte_end, resolved_oid)`.
pub(crate) fn commit_hash_ref_ranges(
    text: &str,
    snap: Option<&jbgit_core::RepoSnapshot>,
) -> Vec<(usize, usize, jbgit_core::Oid)> {
    let Some(snap) = snap else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            let end = i;
            let len = end - start;
            let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if left_ok
                && right_ok
                && (7..=40).contains(&len)
                && let Some(oid) = snap.resolve_revision(&text[start..end])
            {
                out.push((start, end, oid));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Renders one wrapped commit-message line with both issue refs (`#123`, `PROJ-4`)
/// and clickable 7..=40 hex commit hash links highlighted, returning the spans
/// (prefixed with `" "`) plus clickable `(x, width, oid)` hit regions.
pub(crate) fn render_body_chunk_with_links(
    chunk: &str,
    max_width: usize,
    base: Style,
    snap: Option<&jbgit_core::RepoSnapshot>,
    theme: &crate::theme::ThemePalette,
) -> (Vec<Span<'static>>, Vec<(u16, u16, jbgit_core::Oid)>) {
    let link_style = Style::default()
        .fg(theme.fg_accent_link)
        .add_modifier(Modifier::UNDERLINED);
    let mut ranges = issue_ref_ranges(chunk);
    let hash_refs = commit_hash_ref_ranges(chunk, snap);
    let mut hits = Vec::with_capacity(hash_refs.len());
    for &(s, e, oid) in &hash_refs {
        ranges.push((s, e));
        let prefix_w = chunk[..s].width();
        let token_w = chunk[s..e].width();
        if prefix_w < max_width && token_w > 0 {
            let x = 1u16.saturating_add(prefix_w as u16);
            let w = token_w.min(max_width.saturating_sub(prefix_w)) as u16;
            hits.push((x, w, oid));
        }
    }
    ranges.sort_unstable();
    let mut spans = vec![Span::raw(" ")];
    spans.extend(highlight_spans(chunk, &ranges, max_width, base, link_style));
    (spans, hits)
}
