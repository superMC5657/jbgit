use serde::{Deserialize, Serialize};

use super::jgit_config::{GlyphModePref, ThemeFlavor};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeConfig {
    #[serde(default)]
    pub flavor: ThemeFlavor,
    #[serde(default)]
    pub glyphs: GlyphsSection,
    #[serde(default)]
    pub palette: PaletteSection,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeConfigOverlay {
    pub flavor: Option<ThemeFlavor>,
    pub glyphs: Option<GlyphsSectionOverlay>,
    pub palette: Option<PaletteSection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlyphsSectionOverlay {
    pub mode: Option<GlyphModePref>,
}

impl ThemeConfig {
    pub fn merge_overlay(&mut self, overlay: &ThemeConfigOverlay) {
        if let Some(f) = overlay.flavor {
            self.flavor = f;
        }
        if let Some(g) = &overlay.glyphs
            && let Some(m) = g.mode
        {
            self.glyphs.mode = m;
        }
        if let Some(p) = &overlay.palette {
            macro_rules! merge_pal {
                ($($field:ident),* $(,)?) => {
                    $(
                        if p.$field.is_some() {
                            self.palette.$field = p.$field.clone();
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
            );
        }
    }
}
