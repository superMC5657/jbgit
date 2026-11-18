//! Darcula / Islands Dark semantic palette. UI code reads colors only through
//! [`ThemePalette`]; nothing else hardcodes color values.

pub mod branch_colors;
pub mod color;
pub mod glyphs;

use jgit_core::{FileStatusKind, RefGroup, RefGroupKind};
use ratatui::style::Color;

pub use branch_colors::{BranchColorManager, DARCULA_TRACKS, TRACK_COUNT, TRACK_NAMES};
pub use color::rgb_to_ansi256;
pub use glyphs::{GlyphMode, GlyphSymbols};

pub type Rgb = (u8, u8, u8);

/// `bg_canvas` as raw RGB, for contrast checks.
pub const CANVAS_RGB: Rgb = (0x1E, 0x1F, 0x22);

pub fn to_color((r, g, b): Rgb, true_color: bool) -> Color {
    if true_color {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(rgb_to_ansi256(r, g, b))
    }
}

/// One branch track: line color plus the dark container tint behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackColor {
    pub fg: Color,
    pub muted_bg: Color,
}

/// Pill colors for one ref kind (`VcsLogStandardColors.Refs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefStyle {
    /// Left indicator / icon color.
    pub icon: Color,
    pub pill_bg: Color,
    pub pill_fg: Color,
}

/// Canonical visual category for a ref pill (single source of truth for
/// colors, icons, and icon column count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefBadgeKind {
    DetachedHead,
    CurrentHead,
    LocalBranch,
    TrackedPair,
    RemoteBranch,
    Tag,
}

impl RefBadgeKind {
    pub fn from_group(group: &RefGroup) -> Self {
        match group.kind {
            RefGroupKind::DetachedHead => Self::DetachedHead,
            _ if group.with_head => Self::CurrentHead,
            RefGroupKind::LocalBranch => Self::LocalBranch,
            RefGroupKind::TrackedPair => Self::TrackedPair,
            RefGroupKind::RemoteBranch => Self::RemoteBranch,
            RefGroupKind::Tag => Self::Tag,
        }
    }

    pub const fn icon_count(self) -> usize {
        match self {
            Self::TrackedPair => 2,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThemePalette {
    pub true_color: bool,
    pub bg_canvas: Color,
    pub bg_panel: Color,
    pub bg_elevated: Color,
    pub bg_selection_focused: Color,
    pub bg_selection_unfocused: Color,
    pub bg_current_branch_row: Color,
    pub bg_hover: Color,
    pub border_splitter: Color,
    pub border_focused: Color,
    pub fg_primary: Color,
    pub fg_bright: Color,
    pub fg_muted: Color,
    pub fg_accent_link: Color,
    pub status_modified: Color,
    pub status_added: Color,
    pub status_untracked: Color,
    pub status_deleted: Color,
    pub status_renamed: Color,
    pub status_conflicted: Color,
    pub diff_add_bg: Color,
    pub diff_add_word_bg: Color,
    pub diff_del_bg: Color,
    pub diff_del_word_bg: Color,
    pub diff_mod_bg: Color,
    pub ref_head: RefStyle,
    pub ref_detached: RefStyle,
    pub ref_local: RefStyle,
    pub ref_tracked: RefStyle,
    pub ref_remote: RefStyle,
    pub ref_tag: RefStyle,
    pub track_palette: [TrackColor; TRACK_COUNT],
}

impl ThemePalette {
    pub fn detect() -> Self {
        Self::darcula(color::detect_truecolor())
    }

    pub fn darcula(true_color: bool) -> Self {
        let c = |r: u8, g: u8, b: u8| to_color((r, g, b), true_color);
        let style = |icon: Rgb, bg: Rgb, fg: Rgb| RefStyle {
            icon: to_color(icon, true_color),
            pill_bg: to_color(bg, true_color),
            pill_fg: to_color(fg, true_color),
        };

        Self {
            true_color,
            bg_canvas: to_color(CANVAS_RGB, true_color), // #1E1F22
            bg_panel: c(43, 45, 48),                     // #2B2D30
            bg_elevated: c(49, 52, 56),                  // #313438
            bg_selection_focused: c(46, 67, 110),        // #2E436E
            bg_selection_unfocused: c(57, 59, 64),       // #393B40
            bg_current_branch_row: c(37, 41, 48),        // #252930
            bg_hover: c(45, 47, 52),                     // #2D2F34
            border_splitter: c(57, 59, 64),              // #393B40
            border_focused: c(53, 116, 240),             // #3574F0
            fg_primary: c(188, 190, 196),                // #BCBEC4
            fg_bright: c(223, 225, 229),                 // #DFE1E5
            fg_muted: c(111, 115, 122),                  // #6F737A
            fg_accent_link: c(84, 138, 247),             // #548AF7
            status_modified: c(104, 151, 187),           // #6897BB
            status_added: c(98, 151, 85),                // #629755
            status_untracked: c(209, 103, 90),           // #D1675A
            status_deleted: c(128, 128, 128),            // #808080
            status_renamed: c(112, 174, 255),            // #70AEFF
            status_conflicted: c(229, 87, 101),          // #E55765
            diff_add_bg: c(41, 68, 54),                  // #294436
            diff_add_word_bg: c(55, 102, 75),            // #37664B
            diff_del_bg: c(72, 42, 45),                  // #482A2D
            diff_del_word_bg: c(110, 56, 62),            // #6E383E
            diff_mod_bg: c(37, 54, 77),                  // #25364D
            //                 icon                  pill bg               pill fg
            ref_head: style((0xE1, 0xC7, 0x31), (0x3A, 0x37, 0x28), (0xF2, 0xE4, 0x9B)),
            ref_detached: style((0xE5, 0xA9, 0x3B), (0x45, 0x34, 0x22), (0xFF, 0xCC, 0x66)),
            ref_local: style((0x3C, 0xB4, 0x5C), (0x28, 0x3B, 0x2E), (0xD4, 0xE8, 0xD9)),
            // Tracked pairs use the local icon color plus `ref_remote.icon` for the second mark.
            ref_tracked: style((0x3C, 0xB4, 0x5C), (0x2F, 0x34, 0x40), (0xDF, 0xE1, 0xE5)),
            ref_remote: style((0x9F, 0x79, 0xB5), (0x36, 0x2D, 0x3E), (0xDF, 0xCE, 0xF0)),
            ref_tag: style((0x99, 0x99, 0x99), (0x31, 0x33, 0x37), (0xB8, 0xBA, 0xC0)),
            track_palette: DARCULA_TRACKS.map(|(fg, bg)| TrackColor {
                fg: to_color(fg, true_color),
                muted_bg: to_color(bg, true_color),
            }),
        }
    }

    pub fn branch_color_manager(&self) -> BranchColorManager {
        BranchColorManager::new(self.track_palette, self.true_color)
    }

    /// File-name color for a change status (`[M]`, `[A]`, `[?]`, …).
    pub fn status_color(&self, kind: FileStatusKind) -> Color {
        match kind {
            FileStatusKind::Modified | FileStatusKind::Typechange => self.status_modified,
            FileStatusKind::Added => self.status_added,
            FileStatusKind::Deleted => self.status_deleted,
            FileStatusKind::Renamed | FileStatusKind::Copied => self.status_renamed,
            FileStatusKind::Untracked => self.status_untracked,
            FileStatusKind::Conflicted => self.status_conflicted,
            FileStatusKind::Ignored => self.fg_muted,
        }
    }

    /// Pill style for a ref badge category.
    pub fn ref_style(&self, badge: RefBadgeKind) -> RefStyle {
        match badge {
            RefBadgeKind::DetachedHead => self.ref_detached,
            RefBadgeKind::CurrentHead => self.ref_head,
            RefBadgeKind::LocalBranch => self.ref_local,
            RefBadgeKind::TrackedPair => self.ref_tracked,
            RefBadgeKind::RemoteBranch => self.ref_remote,
            RefBadgeKind::Tag => self.ref_tag,
        }
    }

    /// Icon glyph + color pairs for a pill badge (1 or 2 icons).
    pub fn badge_icons(
        &self,
        glyphs: &GlyphSymbols,
        badge: RefBadgeKind,
    ) -> [Option<(&'static str, Color)>; 2] {
        match badge {
            RefBadgeKind::DetachedHead => {
                [Some((glyphs.detached_head, self.ref_detached.icon)), None]
            }
            RefBadgeKind::CurrentHead => [Some((glyphs.head, self.ref_head.icon)), None],
            RefBadgeKind::LocalBranch => {
                [Some((glyphs.local_branch, self.ref_local.icon)), None]
            }
            RefBadgeKind::TrackedPair => [
                Some((glyphs.local_branch, self.ref_local.icon)),
                Some((glyphs.tracked_remote, self.ref_remote.icon)),
            ],
            RefBadgeKind::RemoteBranch => {
                [Some((glyphs.remote_branch, self.ref_remote.icon)), None]
            }
            RefBadgeKind::Tag => [Some((glyphs.tag, self.ref_tag.icon)), None],
        }
    }
}

#[cfg(test)]
mod tests {
    use jgit_core::{LabelDisplay, RefGroup};

    use super::*;

    #[test]
    fn ansi_fallback_never_emits_rgb() {
        let t = ThemePalette::darcula(false);
        let all = [
            t.bg_canvas,
            t.fg_primary,
            t.ref_head.icon,
            t.track_palette[13].fg,
        ];
        assert!(all.iter().all(|c| matches!(c, Color::Indexed(_))));
        let t = ThemePalette::darcula(true);
        assert_eq!(t.bg_selection_focused, Color::Rgb(0x2E, 0x43, 0x6E));
        assert_eq!(t.track_palette[0].fg, Color::Rgb(0xFF, 0x62, 0x85));
    }

    #[test]
    fn head_style_overrides_branch_kind() {
        let t = ThemePalette::darcula(true);
        let mk = |kind, with_head| RefGroup {
            kind,
            label: String::new(),
            with_head,
            refs: Vec::new(),
            display: LabelDisplay::Full,
        };
        assert_eq!(
            t.ref_style(RefBadgeKind::from_group(&mk(RefGroupKind::TrackedPair, true))),
            t.ref_head
        );
        assert_eq!(
            t.ref_style(RefBadgeKind::from_group(&mk(RefGroupKind::TrackedPair, false))),
            t.ref_tracked
        );
        assert_eq!(
            t.ref_style(RefBadgeKind::from_group(&mk(RefGroupKind::DetachedHead, true))),
            t.ref_detached
        );
    }

    #[test]
    fn badge_icon_count_matches_rendered_icons() {
        let t = ThemePalette::darcula(true);
        for mode in [GlyphMode::Unicode, GlyphMode::NerdFont] {
            for badge in [
                RefBadgeKind::DetachedHead,
                RefBadgeKind::CurrentHead,
                RefBadgeKind::LocalBranch,
                RefBadgeKind::TrackedPair,
                RefBadgeKind::RemoteBranch,
                RefBadgeKind::Tag,
            ] {
                let actual = t
                    .badge_icons(mode.symbols(), badge)
                    .into_iter()
                    .flatten()
                    .count();
                assert_eq!(actual, badge.icon_count(), "{mode:?} {badge:?}");
            }
        }
    }
}
