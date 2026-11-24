//! Theme semantic palette. Built-in flavors live in `config/defaults/themes/*.toml`,
//! and UI code reads colors only through [`ThemePalette`].

pub mod branch_colors;
pub mod color;
pub mod glyphs;

use jbgit_core::{FileStatusKind, RefGroup, RefGroupKind};
use ratatui::style::Color;

pub use branch_colors::{BranchColorManager, TRACK_COUNT, TRACK_NAMES};
pub use color::rgb_to_ansi256;
pub use glyphs::{GlyphMode, GlyphSymbols};

pub type Rgb = (u8, u8, u8);

pub fn to_color((r, g, b): Rgb, true_color: bool) -> Color {
    if true_color {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(rgb_to_ansi256(r, g, b))
    }
}

pub fn parse_hex_rgb(hex: &str) -> Option<Rgb> {
    let s = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

pub fn parse_hex_color(hex: &str, true_color: bool) -> Option<Color> {
    parse_hex_rgb(hex).map(|rgb| to_color(rgb, true_color))
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
    pub bg_canvas_rgb: Rgb,
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
    pub annotate_age_0: Color,
    pub annotate_age_1: Color,
    pub annotate_age_2: Color,
    pub annotate_age_3: Color,
    pub annotate_age_4: Color,
    pub ref_head: RefStyle,
    pub ref_detached: RefStyle,
    pub ref_local: RefStyle,
    pub ref_tracked: RefStyle,
    pub ref_remote: RefStyle,
    pub ref_tag: RefStyle,
    pub track_rgbs: [(Rgb, Rgb); TRACK_COUNT],
    pub track_palette: [TrackColor; TRACK_COUNT],
}

fn req_rgb(val: Option<&String>, name: &str) -> Rgb {
    let hex = val.unwrap_or_else(|| panic!("missing required theme key: {name}"));
    parse_hex_rgb(hex).unwrap_or_else(|| panic!("invalid hex color for {name}: {hex}"))
}

fn req_ref_style(
    cfg: Option<&crate::config::RefStyleConfig>,
    name: &str,
    true_color: bool,
) -> RefStyle {
    let c = cfg.unwrap_or_else(|| panic!("missing required ref style: {name}"));
    RefStyle {
        icon: to_color(req_rgb(c.icon.as_ref(), &format!("{name}.icon")), true_color),
        pill_bg: to_color(
            req_rgb(c.pill_bg.as_ref(), &format!("{name}.pill_bg")),
            true_color,
        ),
        pill_fg: to_color(
            req_rgb(c.pill_fg.as_ref(), &format!("{name}.pill_fg")),
            true_color,
        ),
    }
}

fn apply_ref_override(
    dst: &mut RefStyle,
    src: Option<&crate::config::RefStyleConfig>,
    true_color: bool,
) {
    let Some(s) = src else {
        return;
    };
    if let Some(hex) = &s.icon
        && let Some(col) = parse_hex_color(hex, true_color)
    {
        dst.icon = col;
    }
    if let Some(hex) = &s.pill_bg
        && let Some(col) = parse_hex_color(hex, true_color)
    {
        dst.pill_bg = col;
    }
    if let Some(hex) = &s.pill_fg
        && let Some(col) = parse_hex_color(hex, true_color)
    {
        dst.pill_fg = col;
    }
}

impl ThemePalette {
    pub fn detect() -> Self {
        Self::darcula(color::detect_truecolor())
    }

    fn from_flavor_data_complete(
        data: &crate::config::ThemeFlavorData,
        true_color: bool,
    ) -> Self {
        let p = &data.palette;
        let r = &data.refs;
        let bg_canvas_rgb = req_rgb(p.bg_canvas.as_ref(), "palette.bg_canvas");
        let col = |v: Option<&String>, name: &str| to_color(req_rgb(v, name), true_color);

        let src_tracks = data
            .graph
            .tracks
            .as_ref()
            .expect("missing required graph.tracks");
        assert!(
            src_tracks.len() >= TRACK_COUNT,
            "graph.tracks must have at least {TRACK_COUNT} entries"
        );
        let mut track_rgbs = [((0, 0, 0), (0, 0, 0)); TRACK_COUNT];
        let mut track_palette = [TrackColor {
            fg: Color::Reset,
            muted_bg: Color::Reset,
        }; TRACK_COUNT];
        for i in 0..TRACK_COUNT {
            let fg_rgb = req_rgb(src_tracks[i].fg.as_ref(), "graph.tracks.fg");
            let bg_rgb = req_rgb(src_tracks[i].muted_bg.as_ref(), "graph.tracks.muted_bg");
            track_rgbs[i] = (fg_rgb, bg_rgb);
            track_palette[i] = TrackColor {
                fg: to_color(fg_rgb, true_color),
                muted_bg: to_color(bg_rgb, true_color),
            };
        }

        Self {
            true_color,
            bg_canvas_rgb,
            bg_canvas: to_color(bg_canvas_rgb, true_color),
            bg_panel: col(p.bg_panel.as_ref(), "palette.bg_panel"),
            bg_elevated: col(p.bg_elevated.as_ref(), "palette.bg_elevated"),
            bg_selection_focused: col(
                p.bg_selection_focused.as_ref(),
                "palette.bg_selection_focused",
            ),
            bg_selection_unfocused: col(
                p.bg_selection_unfocused.as_ref(),
                "palette.bg_selection_unfocused",
            ),
            bg_current_branch_row: col(
                p.bg_current_branch_row.as_ref(),
                "palette.bg_current_branch_row",
            ),
            bg_hover: col(p.bg_hover.as_ref(), "palette.bg_hover"),
            border_splitter: col(p.border_splitter.as_ref(), "palette.border_splitter"),
            border_focused: col(p.border_focused.as_ref(), "palette.border_focused"),
            fg_primary: col(p.fg_primary.as_ref(), "palette.fg_primary"),
            fg_bright: col(p.fg_bright.as_ref(), "palette.fg_bright"),
            fg_muted: col(p.fg_muted.as_ref(), "palette.fg_muted"),
            fg_accent_link: col(p.fg_accent_link.as_ref(), "palette.fg_accent_link"),
            status_modified: col(p.status_modified.as_ref(), "palette.status_modified"),
            status_added: col(p.status_added.as_ref(), "palette.status_added"),
            status_untracked: col(p.status_untracked.as_ref(), "palette.status_untracked"),
            status_deleted: col(p.status_deleted.as_ref(), "palette.status_deleted"),
            status_renamed: col(p.status_renamed.as_ref(), "palette.status_renamed"),
            status_conflicted: col(p.status_conflicted.as_ref(), "palette.status_conflicted"),
            diff_add_bg: col(p.diff_add_bg.as_ref(), "palette.diff_add_bg"),
            diff_add_word_bg: col(p.diff_add_word_bg.as_ref(), "palette.diff_add_word_bg"),
            diff_del_bg: col(p.diff_del_bg.as_ref(), "palette.diff_del_bg"),
            diff_del_word_bg: col(p.diff_del_word_bg.as_ref(), "palette.diff_del_word_bg"),
            diff_mod_bg: col(p.diff_mod_bg.as_ref(), "palette.diff_mod_bg"),
            annotate_age_0: col(p.annotate_age_0.as_ref(), "palette.annotate_age_0"),
            annotate_age_1: col(p.annotate_age_1.as_ref(), "palette.annotate_age_1"),
            annotate_age_2: col(p.annotate_age_2.as_ref(), "palette.annotate_age_2"),
            annotate_age_3: col(p.annotate_age_3.as_ref(), "palette.annotate_age_3"),
            annotate_age_4: col(p.annotate_age_4.as_ref(), "palette.annotate_age_4"),
            ref_head: req_ref_style(r.head.as_ref(), "refs.head", true_color),
            ref_detached: req_ref_style(r.detached.as_ref(), "refs.detached", true_color),
            ref_local: req_ref_style(r.local.as_ref(), "refs.local", true_color),
            ref_tracked: req_ref_style(r.tracked.as_ref(), "refs.tracked", true_color),
            ref_remote: req_ref_style(r.remote.as_ref(), "refs.remote", true_color),
            ref_tag: req_ref_style(r.tag.as_ref(), "refs.tag", true_color),
            track_rgbs,
            track_palette,
        }
    }

    fn apply_palette_section(&mut self, p: &crate::config::PaletteSection) {
        let tc = self.true_color;
        if let Some(hex) = &p.bg_canvas
            && let Some(rgb) = parse_hex_rgb(hex)
        {
            self.bg_canvas_rgb = rgb;
            self.bg_canvas = to_color(rgb, tc);
        }
        macro_rules! apply_override {
            ($field:ident) => {
                if let Some(hex) = &p.$field
                    && let Some(col) = parse_hex_color(hex, tc)
                {
                    self.$field = col;
                }
            };
        }
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
        apply_override!(diff_mod_bg);
        apply_override!(annotate_age_0);
        apply_override!(annotate_age_1);
        apply_override!(annotate_age_2);
        apply_override!(annotate_age_3);
        apply_override!(annotate_age_4);
    }

    fn apply_refs_section(&mut self, r: &crate::config::RefsSection) {
        let tc = self.true_color;
        apply_ref_override(&mut self.ref_head, r.head.as_ref(), tc);
        apply_ref_override(&mut self.ref_detached, r.detached.as_ref(), tc);
        apply_ref_override(&mut self.ref_local, r.local.as_ref(), tc);
        apply_ref_override(&mut self.ref_tracked, r.tracked.as_ref(), tc);
        apply_ref_override(&mut self.ref_remote, r.remote.as_ref(), tc);
        apply_ref_override(&mut self.ref_tag, r.tag.as_ref(), tc);
    }

    fn apply_graph_section(&mut self, g: &crate::config::GraphThemeSection) {
        let Some(tracks) = &g.tracks else {
            return;
        };
        let tc = self.true_color;
        for (i, t) in tracks.iter().enumerate().take(TRACK_COUNT) {
            if let Some(hex) = &t.fg
                && let Some(rgb) = parse_hex_rgb(hex)
            {
                self.track_rgbs[i].0 = rgb;
                self.track_palette[i].fg = to_color(rgb, tc);
            }
            if let Some(hex) = &t.muted_bg
                && let Some(rgb) = parse_hex_rgb(hex)
            {
                self.track_rgbs[i].1 = rgb;
                self.track_palette[i].muted_bg = to_color(rgb, tc);
            }
        }
    }

    pub fn apply_flavor_data(&mut self, data: &crate::config::ThemeFlavorData) {
        self.apply_palette_section(&data.palette);
        self.apply_refs_section(&data.refs);
        self.apply_graph_section(&data.graph);
    }

    pub fn from_builtin_flavor(flavor: &crate::config::ThemeFlavor, true_color: bool) -> Self {
        use crate::config::{
            BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML, BUILTIN_THEME_DARCULA_TOML,
            BUILTIN_THEME_GRUVBOX_DARK_TOML, ThemeFlavor, ThemeFlavorData,
        };
        let base_data: ThemeFlavorData =
            toml::from_str(BUILTIN_THEME_DARCULA_TOML).expect("valid embedded darcula.toml");
        let mut palette = Self::from_flavor_data_complete(&base_data, true_color);
        let extra_toml = match flavor {
            ThemeFlavor::CatppuccinMocha => Some(BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML),
            ThemeFlavor::GruvboxDark => Some(BUILTIN_THEME_GRUVBOX_DARK_TOML),
            ThemeFlavor::Darcula | ThemeFlavor::Custom(_) => None,
        };
        if let Some(toml_str) = extra_toml
            && let Ok(data) = toml::from_str::<ThemeFlavorData>(toml_str)
        {
            palette.apply_flavor_data(&data);
        }
        palette
    }

    pub fn darcula(true_color: bool) -> Self {
        Self::from_builtin_flavor(&crate::config::ThemeFlavor::Darcula, true_color)
    }

    pub fn catppuccin_mocha(true_color: bool) -> Self {
        Self::from_builtin_flavor(&crate::config::ThemeFlavor::CatppuccinMocha, true_color)
    }

    pub fn gruvbox_dark(true_color: bool) -> Self {
        Self::from_builtin_flavor(&crate::config::ThemeFlavor::GruvboxDark, true_color)
    }

    pub fn from_config(config: &crate::config::ThemeConfig) -> Self {
        Self::from_config_with_truecolor(config, color::detect_truecolor())
    }

    pub fn from_config_with_truecolor(
        config: &crate::config::ThemeConfig,
        true_color: bool,
    ) -> Self {
        let mut palette = Self::from_builtin_flavor(&config.flavor, true_color);
        palette.apply_flavor_data(&config.flavor_overrides);
        palette.apply_palette_section(&config.palette);
        palette.apply_refs_section(&config.refs);
        palette.apply_graph_section(&config.graph);
        palette
    }

    pub const fn annotate_age_color(&self, bucket: usize) -> Color {
        match bucket {
            0 => self.annotate_age_0,
            1 => self.annotate_age_1,
            2 => self.annotate_age_2,
            3 => self.annotate_age_3,
            _ => self.annotate_age_4,
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

    /// Icon foreground and pill colors `(icon_fg, pill_fg, pill_bg)` for a commit signature status.
    pub fn signature_colors(
        &self,
        status: jbgit_core::SignatureStatus,
    ) -> (Color, Color, Color) {
        match status {
            jbgit_core::SignatureStatus::Good => {
                (self.status_added, self.ref_local.pill_fg, self.ref_local.pill_bg)
            }
            jbgit_core::SignatureStatus::GoodUnknownValidity
            | jbgit_core::SignatureStatus::Expired { .. } => (
                self.ref_detached.icon,
                self.ref_detached.pill_fg,
                self.ref_detached.pill_bg,
            ),
            jbgit_core::SignatureStatus::Revoked | jbgit_core::SignatureStatus::Bad => {
                (self.status_conflicted, self.status_conflicted, self.diff_del_bg)
            }
            jbgit_core::SignatureStatus::CannotCheck
            | jbgit_core::SignatureStatus::Pending
            | jbgit_core::SignatureStatus::Unsigned => {
                (self.fg_muted, self.fg_muted, self.bg_elevated)
            }
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
    use jbgit_core::{LabelDisplay, RefGroup};

    use super::*;

    #[test]
    fn ansi_fallback_never_emits_rgb() {
        let t = ThemePalette::darcula(false);
        let all = [
            t.bg_canvas,
            t.fg_primary,
            t.ref_head.icon,
            t.track_palette[13].fg,
            t.annotate_age_0,
            t.annotate_age_1,
            t.annotate_age_2,
            t.annotate_age_3,
            t.annotate_age_4,
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

    #[test]
    fn embedded_theme_flavors_parse_completely_with_valid_hex() {
        use crate::config::{
            BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML, BUILTIN_THEME_DARCULA_TOML,
            BUILTIN_THEME_GRUVBOX_DARK_TOML, ThemeFlavorData,
        };

        for (name, content) in [
            ("darcula", BUILTIN_THEME_DARCULA_TOML),
            ("catppuccin-mocha", BUILTIN_THEME_CATPPUCCIN_MOCHA_TOML),
            ("gruvbox-dark", BUILTIN_THEME_GRUVBOX_DARK_TOML),
        ] {
            let data: ThemeFlavorData =
                toml::from_str(content).unwrap_or_else(|e| panic!("{name}: {e}"));
            let pal = ThemePalette::from_flavor_data_complete(&data, true);
            assert_ne!(pal.bg_canvas_rgb, (0, 0, 0), "{name}");
            assert_eq!(pal.track_rgbs.len(), TRACK_COUNT);
        }
    }
}
