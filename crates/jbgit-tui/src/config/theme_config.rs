use serde::{Deserialize, Serialize};

use super::jbgit_config::{GlyphModePref, ThemeFlavor};
use crate::theme::TRACK_COUNT;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeConfig {
    #[serde(default)]
    pub flavor: ThemeFlavor,
    #[serde(default)]
    pub glyphs: GlyphsSection,
    #[serde(default)]
    pub palette: PaletteSection,
    #[serde(default)]
    pub refs: RefsSection,
    #[serde(default)]
    pub graph: GraphThemeSection,
    /// Layered `themes/<flavor>.toml` overrides populated during `AppConfig::load`.
    #[serde(skip)]
    pub flavor_overrides: ThemeFlavorData,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlyphsSection {
    #[serde(default)]
    pub mode: GlyphModePref,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaletteSection {
    pub bg_canvas: Option<String>,
    pub bg_panel: Option<String>,
    pub bg_elevated: Option<String>,
    pub bg_selection_focused: Option<String>,
    pub bg_selection_unfocused: Option<String>,
    pub bg_current_branch_row: Option<String>,
    pub bg_hover: Option<String>,
    pub border_splitter: Option<String>,
    pub border_focused: Option<String>,
    pub fg_primary: Option<String>,
    pub fg_bright: Option<String>,
    pub fg_muted: Option<String>,
    pub fg_accent_link: Option<String>,
    pub status_modified: Option<String>,
    pub status_added: Option<String>,
    pub status_untracked: Option<String>,
    pub status_deleted: Option<String>,
    pub status_renamed: Option<String>,
    pub status_conflicted: Option<String>,
    pub diff_add_bg: Option<String>,
    pub diff_add_word_bg: Option<String>,
    pub diff_del_bg: Option<String>,
    pub diff_del_word_bg: Option<String>,
    pub diff_mod_bg: Option<String>,
    pub annotate_age_0: Option<String>,
    pub annotate_age_1: Option<String>,
    pub annotate_age_2: Option<String>,
    pub annotate_age_3: Option<String>,
    pub annotate_age_4: Option<String>,
}

impl PaletteSection {
    pub fn merge_from(&mut self, other: &PaletteSection) {
        macro_rules! merge_pal {
            ($($field:ident),* $(,)?) => {
                $(
                    if other.$field.is_some() {
                        self.$field = other.$field.clone();
                    }
                )*
            };
        }
        merge_pal!(
            bg_canvas,
            bg_panel,
            bg_elevated,
            bg_selection_focused,
            bg_selection_unfocused,
            bg_current_branch_row,
            bg_hover,
            border_splitter,
            border_focused,
            fg_primary,
            fg_bright,
            fg_muted,
            fg_accent_link,
            status_modified,
            status_added,
            status_untracked,
            status_deleted,
            status_renamed,
            status_conflicted,
            diff_add_bg,
            diff_add_word_bg,
            diff_del_bg,
            diff_del_word_bg,
            diff_mod_bg,
            annotate_age_0,
            annotate_age_1,
            annotate_age_2,
            annotate_age_3,
            annotate_age_4,
        );
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefStyleConfig {
    pub icon: Option<String>,
    pub pill_bg: Option<String>,
    pub pill_fg: Option<String>,
}

impl RefStyleConfig {
    pub fn merge_from(&mut self, other: &RefStyleConfig) {
        if other.icon.is_some() {
            self.icon = other.icon.clone();
        }
        if other.pill_bg.is_some() {
            self.pill_bg = other.pill_bg.clone();
        }
        if other.pill_fg.is_some() {
            self.pill_fg = other.pill_fg.clone();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefsSection {
    pub head: Option<RefStyleConfig>,
    pub detached: Option<RefStyleConfig>,
    pub local: Option<RefStyleConfig>,
    pub tracked: Option<RefStyleConfig>,
    pub remote: Option<RefStyleConfig>,
    pub tag: Option<RefStyleConfig>,
}

impl RefsSection {
    pub fn merge_from(&mut self, other: &RefsSection) {
        macro_rules! merge_ref {
            ($($field:ident),* $(,)?) => {
                $(
                    if let Some(src) = &other.$field {
                        match &mut self.$field {
                            Some(dst) => dst.merge_from(src),
                            None => self.$field = Some(src.clone()),
                        }
                    }
                )*
            };
        }
        merge_ref!(head, detached, local, tracked, remote, tag);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackColorConfig {
    pub fg: Option<String>,
    pub muted_bg: Option<String>,
}

impl TrackColorConfig {
    pub fn merge_from(&mut self, other: &TrackColorConfig) {
        if other.fg.is_some() {
            self.fg = other.fg.clone();
        }
        if other.muted_bg.is_some() {
            self.muted_bg = other.muted_bg.clone();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphThemeSection {
    pub tracks: Option<Vec<TrackColorConfig>>,
}

impl GraphThemeSection {
    pub fn merge_from(&mut self, other: &GraphThemeSection) {
        let Some(src_tracks) = &other.tracks else {
            return;
        };
        let dst_tracks = self.tracks.get_or_insert_with(Vec::new);
        let limit = src_tracks.len().min(TRACK_COUNT);
        if dst_tracks.len() < limit {
            dst_tracks.resize_with(limit, TrackColorConfig::default);
        }
        for (dst, src) in dst_tracks
            .iter_mut()
            .zip(src_tracks.iter())
            .take(TRACK_COUNT)
        {
            dst.merge_from(src);
        }
    }
}

/// Schema of `themes/<flavor>.toml` flavor files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeFlavorData {
    #[serde(default)]
    pub palette: PaletteSection,
    #[serde(default)]
    pub refs: RefsSection,
    #[serde(default)]
    pub graph: GraphThemeSection,
}

impl ThemeFlavorData {
    pub fn merge_from(&mut self, other: &ThemeFlavorData) {
        self.palette.merge_from(&other.palette);
        self.refs.merge_from(&other.refs);
        self.graph.merge_from(&other.graph);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeConfigOverlay {
    pub flavor: Option<ThemeFlavor>,
    pub glyphs: Option<GlyphsSectionOverlay>,
    pub palette: Option<PaletteSection>,
    pub refs: Option<RefsSection>,
    pub graph: Option<GraphThemeSection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlyphsSectionOverlay {
    pub mode: Option<GlyphModePref>,
}

impl ThemeConfig {
    pub fn merge_overlay(&mut self, overlay: &ThemeConfigOverlay) {
        if let Some(f) = &overlay.flavor {
            self.flavor = f.clone();
        }
        if let Some(g) = &overlay.glyphs
            && let Some(m) = g.mode
        {
            self.glyphs.mode = m;
        }
        if let Some(p) = &overlay.palette {
            self.palette.merge_from(p);
        }
        if let Some(r) = &overlay.refs {
            self.refs.merge_from(r);
        }
        if let Some(g) = &overlay.graph {
            self.graph.merge_from(g);
        }
    }
}
