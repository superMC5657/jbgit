//! Darcula / Islands Dark semantic palette. UI code reads colors only through
//! [`ThemePalette`]; nothing else hardcodes color values.

pub mod branch_colors;
pub mod color;
pub mod glyphs;

use jgit_core::{FileStatusKind, RefGroup, RefGroupKind};
use ratatui::style::Color;

pub use branch_colors::{
    BranchColorManager, CATPPUCCIN_TRACKS, DARCULA_TRACKS, GRUVBOX_TRACKS, TRACK_COUNT, TRACK_NAMES,
};
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

pub fn parse_hex_color(hex: &str, true_color: bool) -> Option<Color> {
    let s = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(to_color((r, g, b), true_color))
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

    pub fn catppuccin_mocha(true_color: bool) -> Self {
        let c = |r: u8, g: u8, b: u8| to_color((r, g, b), true_color);
        let style = |icon: Rgb, bg: Rgb, fg: Rgb| RefStyle {
            icon: to_color(icon, true_color),
            pill_bg: to_color(bg, true_color),
            pill_fg: to_color(fg, true_color),
        };

        Self {
            true_color,
            bg_canvas: c(0x1E, 0x1E, 0x2E),              // #1E1E2E Base
            bg_panel: c(0x18, 0x18, 0x25),               // #181825 Mantle
            bg_elevated: c(0x31, 0x32, 0x44),            // #313244 Surface0
            bg_selection_focused: c(0x45, 0x47, 0x5A),   // #45475A Surface1
            bg_selection_unfocused: c(0x31, 0x32, 0x44), // #313244 Surface0
            bg_current_branch_row: c(0x2A, 0x2B, 0x3D),
            bg_hover: c(0x31, 0x32, 0x44),
            border_splitter: c(0x31, 0x32, 0x44),        // #313244 Surface0
            border_focused: c(0x89, 0xB4, 0xFA),         // #89B4FA Blue
            fg_primary: c(0xCD, 0xD6, 0xF4),             // #CDD6F4 Text
            fg_bright: c(0xF5, 0xE0, 0xDC),              // #F5E0DC Rosewater
            fg_muted: c(0x7F, 0x84, 0x9C),               // #7F849C Subtext0
            fg_accent_link: c(0x89, 0xB4, 0xFA),         // #89B4FA Blue
            status_modified: c(0x89, 0xDC, 0xEB),        // #89DCEB Sky
            status_added: c(0xA6, 0xE3, 0xA1),           // #A6E3A1 Green
            status_untracked: c(0x6C, 0x70, 0x86),       // #6C7086 Overlay0
            status_deleted: c(0xF3, 0x8B, 0xA8),         // #F38BA8 Red
            status_renamed: c(0xCB, 0xA6, 0xF7),         // #CBA6F7 Mauve
            status_conflicted: c(0xEB, 0xA0, 0xAC),       // #EBA0AC Maroon
            diff_add_bg: c(0x24, 0x38, 0x2A),
            diff_add_word_bg: c(0x2E, 0x52, 0x3A),
            diff_del_bg: c(0x3E, 0x23, 0x2B),
            diff_del_word_bg: c(0x59, 0x2C, 0x37),
            diff_mod_bg: c(0x24, 0x2E, 0x3D),
            ref_head: style((0xF9, 0xE2, 0xAF), (0x3E, 0x38, 0x2A), (0xF9, 0xE2, 0xAF)),
            ref_detached: style((0xFA, 0xB3, 0x87), (0x42, 0x32, 0x26), (0xFA, 0xB3, 0x87)),
            ref_local: style((0xA6, 0xE3, 0xA1), (0x24, 0x38, 0x2A), (0xA6, 0xE3, 0xA1)),
            ref_tracked: style((0xA6, 0xE3, 0xA1), (0x31, 0x32, 0x44), (0xCD, 0xD6, 0xF4)),
            ref_remote: style((0xCB, 0xA6, 0xF7), (0x33, 0x28, 0x40), (0xCB, 0xA6, 0xF7)),
            ref_tag: style((0x93, 0x99, 0xB2), (0x31, 0x32, 0x44), (0xCD, 0xD6, 0xF4)),
            track_palette: CATPPUCCIN_TRACKS.map(|(fg, bg)| TrackColor {
                fg: to_color(fg, true_color),
                muted_bg: to_color(bg, true_color),
            }),
        }
    }

    pub fn gruvbox_dark(true_color: bool) -> Self {
        let c = |r: u8, g: u8, b: u8| to_color((r, g, b), true_color);
        let style = |icon: Rgb, bg: Rgb, fg: Rgb| RefStyle {
            icon: to_color(icon, true_color),
            pill_bg: to_color(bg, true_color),
            pill_fg: to_color(fg, true_color),
        };

        Self {
            true_color,
            bg_canvas: c(0x28, 0x28, 0x28),              // #282828 bg0
            bg_panel: c(0x3C, 0x38, 0x36),               // #3C3836 bg1
            bg_elevated: c(0x50, 0x49, 0x45),            // #504945 bg2
            bg_selection_focused: c(0x50, 0x49, 0x45),   // #504945
            bg_selection_unfocused: c(0x3C, 0x38, 0x36), // #3C3836
            bg_current_branch_row: c(0x32, 0x30, 0x2F),
            bg_hover: c(0x3C, 0x38, 0x36),
            border_splitter: c(0x50, 0x49, 0x45),
            border_focused: c(0x83, 0xA5, 0x98),         // #83A598 Blue
            fg_primary: c(0xEB, 0xDB, 0xB2),             // #EBDBB2 Light1
            fg_bright: c(0xFB, 0xF1, 0xC7),              // #FBF1C7 Light0
            fg_muted: c(0x92, 0x83, 0x74),               // #928374 Gray
            fg_accent_link: c(0x83, 0xA5, 0x98),         // #83A598 Blue
            status_modified: c(0x83, 0xA5, 0x98),        // #83A598 Blue
            status_added: c(0xB8, 0xBB, 0x26),           // #B8BB26 Green
            status_untracked: c(0x92, 0x83, 0x74),       // #928374 Gray
            status_deleted: c(0xFB, 0x49, 0x34),         // #FB4934 Red
            status_renamed: c(0xD3, 0x86, 0x9B),         // #D3869B Purple
            status_conflicted: c(0xCC, 0x24, 0x1D),       // #CC241D Red
            diff_add_bg: c(0x2B, 0x38, 0x28),
            diff_add_word_bg: c(0x3B, 0x52, 0x34),
            diff_del_bg: c(0x3C, 0x24, 0x24),
            diff_del_word_bg: c(0x59, 0x2D, 0x2D),
            diff_mod_bg: c(0x26, 0x36, 0x3C),
            ref_head: style((0xFA, 0xBD, 0x2F), (0x3C, 0x34, 0x24), (0xFA, 0xBD, 0x2F)),
            ref_detached: style((0xFE, 0x80, 0x19), (0x3E, 0x2B, 0x20), (0xFE, 0x80, 0x19)),
            ref_local: style((0xB8, 0xBB, 0x26), (0x2B, 0x38, 0x28), (0xB8, 0xBB, 0x26)),
            ref_tracked: style((0xB8, 0xBB, 0x26), (0x3C, 0x38, 0x36), (0xEB, 0xDB, 0xB2)),
            ref_remote: style((0xD3, 0x86, 0x9B), (0x39, 0x2A, 0x36), (0xD3, 0x86, 0x9B)),
            ref_tag: style((0xA8, 0x99, 0x84), (0x3C, 0x38, 0x36), (0xEB, 0xDB, 0xB2)),
            track_palette: GRUVBOX_TRACKS.map(|(fg, bg)| TrackColor {
                fg: to_color(fg, true_color),
                muted_bg: to_color(bg, true_color),
            }),
        }
    }

    pub fn from_config(config: &crate::config::ThemeConfig) -> Self {
        Self::from_config_with_truecolor(config, color::detect_truecolor())
    }

    pub fn from_config_with_truecolor(
        config: &crate::config::ThemeConfig,
        true_color: bool,
    ) -> Self {
        use crate::config::ThemeFlavor;
        let mut palette = match config.flavor {
            ThemeFlavor::CatppuccinMocha => Self::catppuccin_mocha(true_color),
            ThemeFlavor::GruvboxDark => Self::gruvbox_dark(true_color),
            ThemeFlavor::Darcula => Self::darcula(true_color),
        };

        let p = &config.palette;
        macro_rules! apply_override {
            ($field:ident) => {
                if let Some(hex) = &p.$field {
                    if let Some(col) = parse_hex_color(hex, true_color) {
                        palette.$field = col;
                    }
                }
            };
        }
        apply_override!(bg_canvas);
        apply_override!(bg_panel);
        apply_override!(bg_elevated);
        apply_override!(bg_selection_focused);
        apply_override!(bg_selection_unfocused);
        apply_override!(bg_current_branch_row);
        apply_override!(bg_hover);
        apply_override!(border_splitter);
        apply_override!(border_focused);
        apply_override!(fg_primary);
        apply_override!(fg_bright);
        apply_override!(fg_muted);
        apply_override!(fg_accent_link);
        apply_override!(status_modified);
        apply_override!(status_added);
        apply_override!(status_untracked);
        apply_override!(status_deleted);
        apply_override!(status_renamed);
        apply_override!(status_conflicted);
        apply_override!(diff_add_bg);
        apply_override!(diff_add_word_bg);
        apply_override!(diff_del_bg);
        apply_override!(diff_del_word_bg);

        palette
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
            RefBadgeKind::LocalBranch => [Some((glyphs.local_branch, self.ref_local.icon)), None],
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
            t.ref_style(RefBadgeKind::from_group(&mk(
                RefGroupKind::TrackedPair,
                true
            ))),
            t.ref_head
        );
        assert_eq!(
            t.ref_style(RefBadgeKind::from_group(&mk(
                RefGroupKind::TrackedPair,
                false
            ))),
            t.ref_tracked
        );
        assert_eq!(
            t.ref_style(RefBadgeKind::from_group(&mk(
                RefGroupKind::DetachedHead,
                true
            ))),
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

    #[test]
    fn theme_from_config_flavors_and_overrides() {
        use crate::config::{ThemeConfig, ThemeFlavor};

        let mut config = ThemeConfig::default();
        let palette = ThemePalette::from_config_with_truecolor(&config, true);
        assert_eq!(palette.bg_canvas, Color::Rgb(0x1E, 0x1F, 0x22)); // darcula

        config.flavor = ThemeFlavor::CatppuccinMocha;
        let palette = ThemePalette::from_config_with_truecolor(&config, true);
        assert_eq!(palette.bg_canvas, Color::Rgb(0x1E, 0x1E, 0x2E)); // catppuccin base
        assert_eq!(palette.border_focused, Color::Rgb(0x89, 0xB4, 0xFA));

        config.flavor = ThemeFlavor::GruvboxDark;
        let palette = ThemePalette::from_config_with_truecolor(&config, true);
        assert_eq!(palette.bg_canvas, Color::Rgb(0x28, 0x28, 0x28)); // gruvbox bg0
        assert_eq!(palette.border_focused, Color::Rgb(0x83, 0xA5, 0x98));

        // Test hex override
        config.palette.bg_canvas = Some("#000000".to_string());
        config.palette.fg_primary = Some("#ffffff".to_string());
        let palette = ThemePalette::from_config_with_truecolor(&config, true);
        assert_eq!(palette.bg_canvas, Color::Rgb(0, 0, 0));
        assert_eq!(palette.fg_primary, Color::Rgb(255, 255, 255));
    }
}
