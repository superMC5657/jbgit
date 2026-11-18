//! `PillBadgeBar`: IDEA-style rounded ref labels in front of a commit subject
//! (`LabelPainter` port).
//!
//! Layout is a pure function ([`layout_pills`]) so width budgeting, `../`
//! shortening and `+N` overflow can be tested without a terminal; rendering
//! writes cells straight into the buffer and registers one hit region per pill.

use jgit_core::refs::{MAX_REF_LABEL_LEN, MIN_SUBJECT_COLS};
use jgit_core::{LabelDisplay, RefGroup, RefGroupKind, shorten_group_label};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

use crate::hit_test::{HitTarget, HitTestRegistry};
use crate::theme::{GlyphSymbols, RefBadgeKind, ThemePalette};

/// Blank columns between two pills (and after the last one).
pub const PILL_GAP: u16 = 1;
/// A label is only drawn if it can show at least this many columns of text
/// (or its whole text when shorter).
const MIN_LABEL_TEXT_COLS: usize = 4;
const PILL_HIT_Z: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PillItem {
    /// A full pill for `groups[group]` showing `text`.
    Label { group: usize, text: String },
    /// Consecutive icon-only groups (compact view / hidden tag names) in one pill.
    IconCluster { groups: Vec<usize> },
    /// `+K` summary for groups that did not fit.
    Overflow { hidden: Vec<usize> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedPill {
    /// Column offset from the start of the bar.
    pub x: u16,
    pub width: u16,
    pub item: PillItem,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PillLayout {
    pub pills: Vec<PlacedPill>,
    /// Columns consumed, including the gap after the last pill (0 if empty).
    pub width: u16,
}

enum Segment {
    Label(usize),
    Cluster(Vec<usize>),
}

fn overflow_text(count: usize) -> String {
    format!("+{count}")
}

/// Computes which pills fit in a commit row of `row_width` columns, always
/// leaving [`MIN_SUBJECT_COLS`] for the subject.
pub fn layout_pills(groups: &[RefGroup], glyphs: &GlyphSymbols, row_width: u16) -> PillLayout {
    let budget = (row_width as usize).saturating_sub(MIN_SUBJECT_COLS);
    let caps = glyphs.pill_cap_left.width() + glyphs.pill_cap_right.width();
    let gap = PILL_GAP as usize;

    let mut segments: Vec<Segment> = Vec::new();
    for (idx, g) in groups.iter().enumerate() {
        match (g.display, segments.last_mut()) {
            (LabelDisplay::IconOnly, Some(Segment::Cluster(members))) => members.push(idx),
            (LabelDisplay::IconOnly, _) => segments.push(Segment::Cluster(vec![idx])),
            (LabelDisplay::Full, _) => segments.push(Segment::Label(idx)),
        }
    }

    let mut out = PillLayout::default();
    let mut used = 0usize; // columns consumed, including trailing gaps
    let mut consumed_groups = 0usize;

    for seg in &segments {
        let seg_len = match seg {
            Segment::Label(_) => 1,
            Segment::Cluster(m) => m.len(),
        };
        let remaining_after = groups.len() - consumed_groups - seg_len;
        let reserve = if remaining_after > 0 {
            caps + overflow_text(remaining_after).width() + gap
        } else {
            0
        };
        let avail = budget.saturating_sub(used + reserve);

        let placed = match seg {
            Segment::Label(idx) => {
                let g = &groups[*idx];
                let chrome = caps + RefBadgeKind::from_group(g).icon_count() + 1; // icons + space before text
                let text_avail = avail.saturating_sub(chrome + gap);
                let needed = g.label.width().min(MIN_LABEL_TEXT_COLS);
                (text_avail >= needed).then(|| {
                    let text = shorten_group_label(g, MAX_REF_LABEL_LEN, text_avail);
                    let width = chrome + text.width();
                    (width, PillItem::Label { group: *idx, text })
                })
            }
            Segment::Cluster(members) => {
                let width = caps + members.len();
                (width + gap <= avail).then(|| {
                    (
                        width,
                        PillItem::IconCluster {
                            groups: members.clone(),
                        },
                    )
                })
            }
        };

        match placed {
            Some((width, item)) => {
                out.pills.push(PlacedPill {
                    x: used as u16,
                    width: width as u16,
                    item,
                });
                used += width + gap;
                consumed_groups += seg_len;
            }
            None => break,
        }
    }

    if consumed_groups < groups.len() {
        let hidden: Vec<usize> = (consumed_groups..groups.len()).collect();
        let width = caps + overflow_text(hidden.len()).width();
        if used + width + gap <= budget {
            out.pills.push(PlacedPill {
                x: used as u16,
                width: width as u16,
                item: PillItem::Overflow { hidden },
            });
            used += width + gap;
        }
    }

    out.width = used as u16;
    out
}

pub struct PillBadgeBar<'a> {
    pub groups: &'a [RefGroup],
    pub glyphs: &'static GlyphSymbols,
    pub theme: &'a ThemePalette,
    /// Background of the row the bar sits on; end caps blend into it so a
    /// selected row (`#2E436E`) never shows dark corners.
    pub row_bg: Color,
    pub visible_row: usize,
}

impl PillBadgeBar<'_> {
    /// Draws the bar at `area` (first row only) and returns the columns used.
    pub fn render(&self, area: Rect, buf: &mut Buffer, hits: &mut HitTestRegistry) -> u16 {
        if area.width == 0 || area.height == 0 {
            return 0;
        }
        let layout = layout_pills(self.groups, self.glyphs, area.width);
        for pill in &layout.pills {
            let x = area.x.saturating_add(pill.x);
            let rect = Rect::new(x, area.y, pill.width.min(area.right().saturating_sub(x)), 1);
            self.render_pill(rect, &pill.item, buf, hits);
        }
        layout.width.min(area.width)
    }

    fn render_pill(
        &self,
        rect: Rect,
        item: &PillItem,
        buf: &mut Buffer,
        hits: &mut HitTestRegistry,
    ) {
        let theme = self.theme;
        let g = self.glyphs;
        let (pill_bg, pill_fg) = match item {
            PillItem::Label { group, .. } => {
                let badge = RefBadgeKind::from_group(&self.groups[*group]);
                let s = theme.ref_style(badge);
                (s.pill_bg, s.pill_fg)
            }
            PillItem::IconCluster { .. } | PillItem::Overflow { .. } => {
                (theme.bg_elevated, theme.fg_primary)
            }
        };
        let cap_style = Style::default().fg(pill_bg).bg(self.row_bg);
        let right = rect.right();
        let mut cur = Cursor {
            x: rect.x,
            y: rect.y,
            right,
        };

        cur.put(buf, g.pill_cap_left, cap_style);
        match item {
            PillItem::Label { group, text } => {
                let grp = &self.groups[*group];
                let badge = RefBadgeKind::from_group(grp);
                for (icon, color) in theme.badge_icons(g, badge).into_iter().flatten() {
                    cur.put(buf, icon, Style::default().fg(color).bg(pill_bg));
                }
                cur.put(buf, " ", Style::default().bg(pill_bg));
                cur.put(buf, text, Style::default().fg(pill_fg).bg(pill_bg));
                hits.register(
                    rect,
                    PILL_HIT_Z,
                    HitTarget::CommitRefPill {
                        visible_row: self.visible_row,
                        ref_name: grp.primary_ref().to_string(),
                        is_detached_head: badge == RefBadgeKind::DetachedHead,
                    },
                );
            }
            PillItem::IconCluster { groups } => {
                for &idx in groups {
                    let grp = &self.groups[idx];
                    let (glyph, color) = self.compact_icon(grp);
                    let cell_x = cur.x;
                    cur.put(buf, glyph, Style::default().fg(color).bg(pill_bg));
                    hits.register(
                        Rect::new(cell_x, rect.y, 1, 1),
                        PILL_HIT_Z,
                        HitTarget::CommitRefPill {
                            visible_row: self.visible_row,
                            ref_name: grp.primary_ref().to_string(),
                            is_detached_head: grp.kind == RefGroupKind::DetachedHead,
                        },
                    );
                }
            }
            PillItem::Overflow { hidden } => {
                cur.put(
                    buf,
                    &overflow_text(hidden.len()),
                    Style::default().fg(pill_fg).bg(pill_bg),
                );
                hits.register(
                    rect,
                    PILL_HIT_Z,
                    HitTarget::CommitRefOverflow {
                        visible_row: self.visible_row,
                        hidden_refs: hidden
                            .iter()
                            .flat_map(|&i| self.groups[i].refs.iter().cloned())
                            .collect(),
                    },
                );
            }
        }
        cur.put(buf, g.pill_cap_right, cap_style);
    }

    fn compact_icon(&self, grp: &RefGroup) -> (&'static str, Color) {
        let badge = RefBadgeKind::from_group(grp);
        let style = self.theme.ref_style(badge);
        match badge {
            RefBadgeKind::Tag => (self.glyphs.tag, style.icon),
            _ => (self.glyphs.compact_bar, style.icon),
        }
    }
}

/// Width-aware cell writer clipped to `right`.
struct Cursor {
    x: u16,
    y: u16,
    right: u16,
}

impl Cursor {
    fn put(&mut self, buf: &mut Buffer, s: &str, style: Style) {
        if self.x >= self.right {
            return;
        }
        let max = (self.right - self.x) as usize;
        let (nx, _) = buf.set_stringn(self.x, self.y, s, max, style);
        self.x = nx;
    }
}

#[cfg(test)]
mod tests {
    use jgit_core::{GroupOptions, Oid, RefInfo, RefKind, group_refs_for_table};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Block;

    use super::*;
    use crate::theme::GlyphMode;

    fn r(name: &str, kind: RefKind, upstream: Option<&str>, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.into(),
            full_name: name.into(),
            target_oid: Oid::ZERO,
            kind,
            upstream: upstream.map(Into::into),
            ahead_behind: None,
            is_head,
        }
    }

    fn sample_groups() -> Vec<RefGroup> {
        let refs = [
            r("main", RefKind::LocalBranch, Some("origin/main"), true),
            r("origin/main", RefKind::RemoteBranch, None, false),
            r(
                "feature/user-authentication-oauth2",
                RefKind::LocalBranch,
                None,
                false,
            ),
            r("release/2026.11", RefKind::LocalBranch, None, false),
            r("v1.0.0", RefKind::Tag, None, false),
            r("v1.0.1", RefKind::Tag, None, false),
        ];
        group_refs_for_table(&refs, GroupOptions::default())
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }

    /// Renders one commit row through a real `Terminal<TestBackend>` frame.
    fn render_row(
        groups: &[RefGroup],
        mode: GlyphMode,
        width: u16,
        row_bg: Color,
    ) -> (Buffer, u16, HitTestRegistry) {
        let theme = ThemePalette::darcula(true);
        let mut terminal = Terminal::new(TestBackend::new(width.max(1), 1)).unwrap();
        let mut hits = HitTestRegistry::new();
        let mut used = 0;
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, width, 1);
                f.render_widget(Block::default().style(Style::default().bg(row_bg)), area);
                used = PillBadgeBar {
                    groups,
                    glyphs: mode.symbols(),
                    theme: &theme,
                    row_bg,
                    visible_row: 7,
                }
                .render(area, f.buffer_mut(), &mut hits);
            })
            .unwrap();
        (terminal.backend().buffer().clone(), used, hits)
    }

    #[test]
    fn wide_row_shows_every_pill_in_order() {
        let groups = sample_groups();
        let (buf, used, _) = render_row(&groups, GlyphMode::Unicode, 140, Color::Reset);
        let text = row_text(&buf, 0);
        assert!(
            text.starts_with(
                "▐◆ origin & main▌ ▐⎇ feature/user-authentication-oauth2▌ ▐⎇ release/2026.11▌ ▐⌗ v1.0.0▌ ▐⌗ v1.0.1▌ "
            ),
            "{text}"
        );
        assert!(140 - used as usize >= MIN_SUBJECT_COLS);
    }

    #[test]
    fn narrow_row_folds_prefix_and_overflows() {
        let groups = sample_groups();
        // budget = 70 - 24 = 46 columns
        let layout = layout_pills(&groups, GlyphMode::Unicode.symbols(), 70);
        assert!(layout.width as usize <= 70 - MIN_SUBJECT_COLS);
        let (buf, _, hits) = render_row(&groups, GlyphMode::Unicode, 70, Color::Reset);
        let text = row_text(&buf, 0);
        assert!(
            text.starts_with("▐◆ origin & main▌ ▐⎇ ../user-auth"),
            "{text}"
        );
        assert!(text.contains("…▌ ▐+3▌ "), "{text}");

        // The overflow pill is clickable and lists the hidden refs.
        let overflow_x = text.chars().position(|c| c == '+').unwrap() as u16;
        let hit = hits.hit_test(overflow_x, 0).unwrap();
        assert_eq!(
            hit.target,
            HitTarget::CommitRefOverflow {
                visible_row: 7,
                hidden_refs: vec!["release/2026.11".into(), "v1.0.0".into(), "v1.0.1".into()],
            }
        );
    }

    #[test]
    fn subject_reserve_is_respected_at_every_width() {
        let groups = sample_groups();
        for mode in [GlyphMode::Unicode, GlyphMode::NerdFont] {
            for w in 0..160u16 {
                let layout = layout_pills(&groups, mode.symbols(), w);
                let budget = (w as usize).saturating_sub(MIN_SUBJECT_COLS);
                assert!(layout.width as usize <= budget, "w={w} {layout:?}");
                for p in &layout.pills {
                    if let PillItem::Label { text, .. } = &p.item {
                        assert!(text.width() >= 1);
                    }
                }
            }
        }
    }

    #[test]
    fn selected_row_caps_blend_into_selection_background() {
        let theme = ThemePalette::darcula(true);
        let groups = sample_groups();
        let sel = theme.bg_selection_focused;
        let (buf, _, _) = render_row(&groups, GlyphMode::Unicode, 120, sel);
        let left = &buf[(0, 0)];
        assert_eq!(left.symbol(), "▐");
        assert_eq!(left.bg, sel, "cap outer side must use row bg");
        assert_eq!(left.fg, theme.ref_head.pill_bg);
        let right_x = row_text(&buf, 0).chars().position(|c| c == '▌').unwrap() as u16;
        assert_eq!(buf[(right_x, 0)].bg, sel);
        // Gap cell after the pill keeps the selection color too.
        assert_eq!(buf[(right_x + 1, 0)].bg, sel);
        // Pill body uses the HEAD pill colors with the gold icon.
        assert_eq!(buf[(1, 0)].fg, theme.ref_head.icon);
        assert_eq!(buf[(1, 0)].bg, theme.ref_head.pill_bg);
        assert_eq!(buf[(3, 0)].fg, theme.ref_head.pill_fg);
    }

    #[test]
    fn nerd_font_mode_uses_rounded_caps() {
        let groups = sample_groups();
        let (buf, _, _) = render_row(&groups, GlyphMode::NerdFont, 120, Color::Reset);
        assert_eq!(buf[(0, 0)].symbol(), "\u{e0b6}");
        assert_eq!(buf[(1, 0)].symbol(), "\u{f418}");
        let text = row_text(&buf, 0);
        assert!(
            text.starts_with("\u{e0b6}\u{f418} origin & main\u{e0b4} "),
            "{text}"
        );
    }

    #[test]
    fn cjk_names_keep_caps_aligned() {
        let refs = [r("feat/登录优化", RefKind::LocalBranch, None, false)];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        let (buf, used, hits) = render_row(&groups, GlyphMode::Unicode, 60, Color::Reset);
        // ▐ ⎇ ␠ feat/ (5) + 登录优化 (8 cols) ▌ = 17 columns
        assert_eq!(buf[(16, 0)].symbol(), "▌");
        assert_eq!(used, 18);
        let region = hits.hit_test(16, 0).unwrap();
        assert_eq!(region.rect.width, 17);
        assert!(hits.hit_test(17, 0).is_none());
    }

    #[test]
    fn compact_view_renders_icon_cluster_with_per_ref_hits() {
        let refs = [
            r("main", RefKind::LocalBranch, None, true),
            r("dev", RefKind::LocalBranch, None, false),
            r("v1", RefKind::Tag, None, false),
        ];
        let groups = group_refs_for_table(
            &refs,
            GroupOptions {
                compact: true,
                show_tag_names: true,
            },
        );
        let (buf, _, hits) = render_row(&groups, GlyphMode::Unicode, 80, Color::Reset);
        let text = row_text(&buf, 0);
        assert!(text.starts_with("▐◆ main▌ ▐┃⌗▌ "), "{text}");
        let x = text.chars().position(|c| c == '┃').unwrap() as u16;
        assert_eq!(
            hits.hit_test(x, 0).unwrap().target,
            HitTarget::CommitRefPill {
                visible_row: 7,
                ref_name: "dev".into(),
                is_detached_head: false,
            }
        );
        assert_eq!(
            hits.hit_test(x + 1, 0).unwrap().target,
            HitTarget::CommitRefPill {
                visible_row: 7,
                ref_name: "v1".into(),
                is_detached_head: false,
            }
        );
    }

    #[test]
    fn tracked_pair_keeps_remote_prefix_when_shortened_and_renders_dual_dot() {
        let refs = [
            r(
                "feature/user-authentication-oauth2",
                RefKind::LocalBranch,
                Some("origin/feature/user-authentication-oauth2"),
                false,
            ),
            r(
                "origin/feature/user-authentication-oauth2",
                RefKind::RemoteBranch,
                None,
                false,
            ),
        ];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        let (buf, _, _) = render_row(&groups, GlyphMode::NerdFont, 68, Color::Reset);
        let text = row_text(&buf, 0);
        assert!(
            text.starts_with("\u{e0b6}\u{e0a0}\u{f444} origin & ../user-auth"),
            "{text}"
        );
    }

    #[test]
    fn too_narrow_row_renders_nothing() {
        let groups = sample_groups();
        let layout = layout_pills(&groups, GlyphMode::Unicode.symbols(), 26);
        assert!(layout.pills.is_empty());
        assert_eq!(layout.width, 0);
    }
}
