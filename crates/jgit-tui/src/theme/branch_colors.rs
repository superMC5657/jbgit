//! Darcula 14-color branch track palette and the color-id → color resolver
//! (`GraphColorManagerImpl` + `DefaultColorGenerator` port).

use jgit_core::{BranchColorId, branch_color_id};
use ratatui::style::Color;
use rustc_hash::FxHashMap;

use super::color::{hsb_to_rgb, rgb_to_hsb};
use super::{Rgb, TrackColor, to_color};

pub const TRACK_COUNT: usize = 14;

/// `(track_fg, muted_bg)` per slot, aligned with JetBrains New UI hues.
///
/// Slot 2 (Indigo) is lifted from the spec's `#7B6FF6` (4.27:1) to `#8176F6`
/// (4.60:1) — same hue, slightly lower saturation — so every track color
/// clears WCAG 4.5:1 against `bg_canvas`.
pub const DARCULA_TRACKS: [(Rgb, Rgb); TRACK_COUNT] = [
    ((0xFF, 0x62, 0x85), (0x60, 0x32, 0x39)), // 0  Rose / Coral
    ((0x57, 0xD1, 0x85), (0x37, 0x52, 0x39)), // 1  Sage / Emerald
    ((0x81, 0x76, 0xF6), (0x38, 0x32, 0x75)), // 2  Indigo / Iris
    ((0xF2, 0xB1, 0x81), (0x61, 0x44, 0x38)), // 3  Apricot / Amber
    ((0x32, 0xE0, 0xD9), (0x1D, 0x4D, 0x4A)), // 4  Cyan / Teal
    ((0xFE, 0xD2, 0x77), (0x5E, 0x4D, 0x33)), // 5  Gold / Sand
    ((0xF4, 0x6B, 0xFF), (0x56, 0x2B, 0x58)), // 6  Magenta / Orchid
    ((0xCE, 0x9E, 0x7A), (0x62, 0x4E, 0x3F)), // 7  Bronze / Ochre
    ((0x4F, 0xF2, 0xB7), (0x0D, 0x57, 0x3C)), // 8  Mint / Spring
    ((0xA1, 0x77, 0xF4), (0x3B, 0x31, 0x47)), // 9  Violet / Lavender
    ((0x46, 0xC2, 0xC7), (0x20, 0x41, 0x45)), // 10 Ocean / Cerulean
    ((0xC9, 0x85, 0xC9), (0x50, 0x32, 0x50)), // 11 Plum / Mauve
    ((0xEC, 0xCA, 0x88), (0x59, 0x4D, 0x36)), // 12 Wheat / Honey
    ((0xB8, 0xBD, 0xC9), (0x49, 0x4B, 0x57)), // 13 Slate / Silver
];

pub const TRACK_NAMES: [&str; TRACK_COUNT] = [
    "Rose", "Sage", "Indigo", "Apricot", "Cyan", "Gold", "Magenta", "Bronze", "Mint", "Violet",
    "Ocean", "Plum", "Wheat", "Slate",
];

pub const CATPPUCCIN_TRACKS: [(Rgb, Rgb); TRACK_COUNT] = [
    ((0xF3, 0x8B, 0xA8), (0x54, 0x2A, 0x38)), // 0 Red
    ((0xFA, 0xB3, 0x87), (0x57, 0x38, 0x28)), // 1 Peach
    ((0xF9, 0xE2, 0xAF), (0x57, 0x4D, 0x32)), // 2 Yellow
    ((0xA6, 0xE3, 0xA1), (0x32, 0x4E, 0x32)), // 3 Green
    ((0x74, 0xC7, 0xEC), (0x24, 0x42, 0x52)), // 4 Sapphire
    ((0x89, 0xB4, 0xFA), (0x2B, 0x3C, 0x58)), // 5 Blue
    ((0xCB, 0xA6, 0xF7), (0x46, 0x34, 0x59)), // 6 Mauve
    ((0xF5, 0xC2, 0xE7), (0x54, 0x38, 0x4E)), // 7 Pink
    ((0xF2, 0xCD, 0xCD), (0x53, 0x40, 0x44)), // 8 Flamingo
    ((0x94, 0xE2, 0xD5), (0x2A, 0x4C, 0x48)), // 9 Teal
    ((0x89, 0xDC, 0xEB), (0x2B, 0x4B, 0x53)), // 10 Sky
    ((0xB4, 0xBE, 0xFE), (0x3B, 0x40, 0x5C)), // 11 Lavender
    ((0xEB, 0xA0, 0xAC), (0x52, 0x33, 0x3A)), // 12 Maroon
    ((0xF5, 0xE0, 0xDC), (0x55, 0x49, 0x49)), // 13 Rosewater
];

pub const GRUVBOX_TRACKS: [(Rgb, Rgb); TRACK_COUNT] = [
    ((0xFB, 0x49, 0x34), (0x57, 0x22, 0x1E)), // 0 Bright Red
    ((0xB8, 0xBB, 0x26), (0x44, 0x48, 0x1E)), // 1 Bright Green
    ((0xFA, 0xBD, 0x2F), (0x56, 0x45, 0x1E)), // 2 Bright Yellow
    ((0x83, 0xA5, 0x98), (0x2E, 0x40, 0x3C)), // 3 Bright Blue
    ((0xD3, 0x86, 0x9B), (0x4E, 0x30, 0x3A)), // 4 Bright Purple
    ((0x8E, 0xC0, 0x7C), (0x33, 0x46, 0x32)), // 5 Bright Aqua
    ((0xFE, 0x80, 0x19), (0x58, 0x33, 0x1A)), // 6 Bright Orange
    ((0xCC, 0x24, 0x1D), (0x49, 0x1D, 0x1A)), // 7 Neutral Red
    ((0x98, 0x97, 0x1A), (0x3B, 0x3C, 0x1B)), // 8 Neutral Green
    ((0xD7, 0x99, 0x21), (0x4E, 0x3B, 0x1B)), // 9 Neutral Yellow
    ((0x45, 0x85, 0x88), (0x20, 0x37, 0x39)), // 10 Neutral Blue
    ((0xB1, 0x62, 0x86), (0x43, 0x28, 0x35)), // 11 Neutral Purple
    ((0x68, 0x9D, 0x6A), (0x2B, 0x3C, 0x2D)), // 12 Neutral Aqua
    ((0xD6, 0x5D, 0x0E), (0x4E, 0x29, 0x16)), // 13 Neutral Orange
];

const FALLBACK_SATURATION: f32 = 0.50;
const FALLBACK_BRIGHTNESS: f32 = 0.78;

/// Palette slot for a graph color id (handles negative hashes).
pub fn palette_slot(color_id: BranchColorId) -> usize {
    color_id.raw().rem_euclid(TRACK_COUNT as i32) as usize
}

/// `DefaultColorGenerator.calcColor` with jGit's dark-theme saturation /
/// brightness. Wrapping arithmetic mirrors JVM `int` overflow.
pub fn hsb_fallback_rgb(color_id: BranchColorId) -> Rgb {
    let id = color_id.raw();
    let range_fix = |n: i32| ((n % 100).abs() + 70) as u8;
    let r = id.wrapping_mul(200).wrapping_add(30);
    let g = id.wrapping_mul(130).wrapping_add(50);
    let b = id.wrapping_mul(90).wrapping_add(100);
    let (hue, _, _) = rgb_to_hsb(range_fix(r), range_fix(g), range_fix(b));
    hsb_to_rgb(hue, FALLBACK_SATURATION, FALLBACK_BRIGHTNESS)
}

#[derive(Debug, Clone)]
pub struct BranchColorManager {
    palette_14: [TrackColor; TRACK_COUNT],
    true_color: bool,
}

impl BranchColorManager {
    pub fn new(palette_14: [TrackColor; TRACK_COUNT], true_color: bool) -> Self {
        Self {
            palette_14,
            true_color,
        }
    }

    pub fn track(&self, slot: usize) -> TrackColor {
        self.palette_14[slot % TRACK_COUNT]
    }

    /// Palette color for `color_id` (no collision handling).
    pub fn get_track_color(&self, color_id: BranchColorId) -> Color {
        self.palette_14[palette_slot(color_id)].fg
    }

    /// Direct palette color for a branch name (`origin/x` and `x` share a color;
    /// `main` / `master` pin to slot 0).
    #[allow(dead_code)]
    pub fn track_color_for_branch(&self, ref_name: &str) -> Color {
        self.get_track_color(branch_color_id(ref_name))
    }

    /// HSB-generated color used once all 14 slots are taken.
    pub fn fallback_color(&self, color_id: BranchColorId) -> Color {
        to_color(hsb_fallback_rgb(color_id), self.true_color)
    }

    /// Row/window-level allocation: equal ids share a color, a colliding id
    /// shifts to the next free slot, and once all 14 slots are busy the HSB
    /// generator takes over so no track ever disappears.
    pub fn assign_visible(&self, color_ids: &[BranchColorId]) -> Vec<Color> {
        let mut slot_owner: [Option<BranchColorId>; TRACK_COUNT] = [None; TRACK_COUNT];
        let mut assigned: FxHashMap<BranchColorId, Color> = FxHashMap::default();
        color_ids
            .iter()
            .map(|&id| {
                if let Some(&c) = assigned.get(&id) {
                    return c;
                }
                let start = palette_slot(id);
                let free = (0..TRACK_COUNT)
                    .map(|k| (start + k) % TRACK_COUNT)
                    .find(|&s| slot_owner[s].is_none());
                let color = match free {
                    Some(slot) => {
                        slot_owner[slot] = Some(id);
                        self.palette_14[slot].fg
                    }
                    None => self.fallback_color(id),
                };
                assigned.insert(id, color);
                color
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::color::{contrast_ratio, relative_luminance};
    use super::super::{CANVAS_RGB, ThemePalette};
    use super::*;

    fn manager() -> BranchColorManager {
        ThemePalette::darcula(true).branch_color_manager()
    }

    #[test]
    fn local_and_remote_share_slot() {
        assert_eq!(palette_slot(branch_color_id("main")), 0);
        assert_eq!(palette_slot(branch_color_id("origin/main")), 0);
        assert_eq!(palette_slot(branch_color_id("master")), 0);
        assert_eq!(
            palette_slot(branch_color_id("feature/auth")),
            palette_slot(branch_color_id("origin/feature/auth"))
        );
        let m = manager();
        assert_eq!(
            m.track_color_for_branch("feature/auth"),
            m.track_color_for_branch("origin/feature/auth")
        );
    }

    #[test]
    fn every_track_meets_wcag_aa_on_canvas() {
        for (i, (fg, _)) in DARCULA_TRACKS.iter().enumerate() {
            let ratio = contrast_ratio(*fg, CANVAS_RGB);
            assert!(
                ratio >= 4.5,
                "track {i} ({}) contrast {ratio:.2} < 4.5",
                TRACK_NAMES[i]
            );
        }
    }

    #[test]
    fn muted_backgrounds_are_darker_than_tracks() {
        for (fg, bg) in DARCULA_TRACKS {
            assert!(relative_luminance(bg) < relative_luminance(fg));
        }
    }

    #[test]
    fn hsb_fallback_is_total_and_never_dark() {
        let probes = [
            i32::MIN,
            i32::MIN + 1,
            -1_000_003,
            -14,
            -1,
            0,
            1,
            13,
            14,
            i32::MAX,
        ];
        let ids = probes.into_iter().chain(-500..500);
        for raw in ids {
            let rgb = hsb_fallback_rgb(BranchColorId(raw));
            let max = rgb.0.max(rgb.1).max(rgb.2);
            // brightness 0.78 → max channel ≈ 199
            assert!((197..=201).contains(&max), "id {raw} → {rgb:?}");
            assert!(
                contrast_ratio(rgb, CANVAS_RGB) >= 3.0,
                "id {raw} → {rgb:?} too dark for a graph line"
            );
            assert_ne!(rgb, (255, 255, 255));
        }
    }

    #[test]
    fn negative_ids_map_into_palette() {
        for raw in [i32::MIN, -1, -13, -14, -15] {
            assert!(palette_slot(BranchColorId(raw)) < TRACK_COUNT);
        }
    }

    #[test]
    fn visible_assignment_shifts_collisions_then_falls_back() {
        let m = manager();
        // 0 and 14 both hash to slot 0 → the second shifts to slot 1.
        let colors = m.assign_visible(&[BranchColorId(0), BranchColorId(14), BranchColorId(0)]);
        assert_eq!(colors[0], m.track(0).fg);
        assert_eq!(colors[1], m.track(1).fg);
        assert_eq!(colors[2], colors[0], "same id keeps its color");

        // 15 distinct ids all hashing to slot 0: 14 palette colors, then HSB.
        let ids: Vec<BranchColorId> = (0..15).map(|k| BranchColorId(k * 14)).collect();
        let colors = m.assign_visible(&ids);
        let palette: Vec<Color> = (0..TRACK_COUNT).map(|s| m.track(s).fg).collect();
        for c in &colors[..14] {
            assert!(palette.contains(c));
        }
        assert_eq!(colors[14], m.fallback_color(BranchColorId(14 * 14)));
        assert!(!palette.contains(&colors[14]));
    }
}
