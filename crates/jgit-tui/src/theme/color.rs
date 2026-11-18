//! Color science helpers: TrueColor detection, CIE76 ANSI-256 downgrade,
//! WCAG contrast and Java-compatible HSB conversions.

use std::sync::LazyLock;

/// Detects 24-bit color support from `COLORTERM`, Windows Terminal (`WT_SESSION`),
/// and well-known `TERM_PROGRAM` values.
pub fn detect_truecolor() -> bool {
    let var = |k: &str| std::env::var(k).ok();
    truecolor_from_env(
        var("COLORTERM").as_deref(),
        std::env::var_os("WT_SESSION").is_some(),
        var("TERM_PROGRAM").as_deref(),
    )
}

fn truecolor_from_env(
    colorterm: Option<&str>,
    wt_session: bool,
    term_program: Option<&str>,
) -> bool {
    if colorterm.is_some_and(|ct| {
        let low = ct.to_ascii_lowercase();
        low.contains("truecolor") || low.contains("24bit")
    }) {
        return true;
    }
    if wt_session {
        return true;
    }
    term_program.is_some_and(|tp| {
        matches!(
            tp,
            "WezTerm" | "iTerm.app" | "vscode" | "Alacritty" | "ghostty" | "Hyper" | "Tabby"
        )
    })
}

// ---------------------------------------------------------------------------
// CIE76 nearest-neighbour ANSI-256 mapping
// ---------------------------------------------------------------------------

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// xterm palette indices 16..=255 (the 6×6×6 cube + 24 grays). Indices 0..16
/// are user-themable and therefore never targeted.
fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    if index >= 232 {
        let v = 8 + 10 * (index - 232);
        return (v, v, v);
    }
    let i = index - 16;
    (
        CUBE_LEVELS[(i / 36) as usize],
        CUBE_LEVELS[((i / 6) % 6) as usize],
        CUBE_LEVELS[(i % 6) as usize],
    )
}

/// Precomputed CIE L*a*b* coordinates of xterm colors 16..=255.
static XTERM_LAB: LazyLock<Vec<(u8, [f32; 3])>> = LazyLock::new(|| {
    (16u8..=255)
        .map(|idx| {
            let (r, g, b) = xterm_rgb(idx);
            (idx, srgb_to_lab(r, g, b))
        })
        .collect()
});

/// Maps an sRGB color to the perceptually nearest xterm-256 index (ΔE*76).
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let lab = srgb_to_lab(r, g, b);
    let mut best = (16u8, f32::MAX);
    for &(idx, cand) in XTERM_LAB.iter() {
        let d =
            (lab[0] - cand[0]).powi(2) + (lab[1] - cand[1]).powi(2) + (lab[2] - cand[2]).powi(2);
        if d < best.1 {
            best = (idx, d);
        }
    }
    best.0
}

fn srgb_channel_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_to_lab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let (r, g, b) = (
        srgb_channel_to_linear(r),
        srgb_channel_to_linear(g),
        srgb_channel_to_linear(b),
    );
    // sRGB → XYZ (D65), normalised by the D65 white point.
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83;
    let f = |t: f32| {
        const EPS: f32 = 216.0 / 24389.0;
        const KAPPA: f32 = 24389.0 / 27.0;
        if t > EPS {
            t.cbrt()
        } else {
            (KAPPA * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

// ---------------------------------------------------------------------------
// WCAG 2.x contrast
// ---------------------------------------------------------------------------

pub fn relative_luminance((r, g, b): (u8, u8, u8)) -> f32 {
    0.2126 * srgb_channel_to_linear(r)
        + 0.7152 * srgb_channel_to_linear(g)
        + 0.0722 * srgb_channel_to_linear(b)
}

pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

// ---------------------------------------------------------------------------
// java.awt.Color HSB conversions
// ---------------------------------------------------------------------------

/// Port of `java.awt.Color.RGBtoHSB`.
pub fn rgb_to_hsb(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let cmax = r.max(g).max(b);
    let cmin = r.min(g).min(b);
    let brightness = cmax / 255.0;
    let saturation = if cmax != 0.0 {
        (cmax - cmin) / cmax
    } else {
        0.0
    };
    let hue = if saturation == 0.0 {
        0.0
    } else {
        let span = cmax - cmin;
        let redc = (cmax - r) / span;
        let greenc = (cmax - g) / span;
        let bluec = (cmax - b) / span;
        let mut h = if r == cmax {
            bluec - greenc
        } else if g == cmax {
            2.0 + redc - bluec
        } else {
            4.0 + greenc - redc
        } / 6.0;
        if h < 0.0 {
            h += 1.0;
        }
        h
    };
    (hue, saturation, brightness)
}

/// Port of `java.awt.Color.HSBtoRGB`.
pub fn hsb_to_rgb(hue: f32, saturation: f32, brightness: f32) -> (u8, u8, u8) {
    let to_u8 = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    if saturation == 0.0 {
        let v = to_u8(brightness);
        return (v, v, v);
    }
    let h = (hue - hue.floor()) * 6.0;
    let f = h - h.floor();
    let p = brightness * (1.0 - saturation);
    let q = brightness * (1.0 - saturation * f);
    let t = brightness * (1.0 - saturation * (1.0 - f));
    let (r, g, b) = match h as i32 {
        0 => (brightness, t, p),
        1 => (q, brightness, p),
        2 => (p, brightness, t),
        3 => (p, q, brightness),
        4 => (t, p, brightness),
        _ => (brightness, p, q),
    };
    (to_u8(r), to_u8(g), to_u8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi256_exact_palette_entries_round_trip() {
        for idx in 16u8..=255 {
            let (r, g, b) = xterm_rgb(idx);
            let mapped = rgb_to_ansi256(r, g, b);
            assert_eq!(xterm_rgb(mapped), (r, g, b), "index {idx}");
        }
        assert_eq!(rgb_to_ansi256(95, 135, 175), 67);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
    }

    #[test]
    fn ansi256_keeps_darcula_layers_distinct() {
        // Canvas / panel / selection must stay distinguishable after downgrade.
        let canvas = rgb_to_ansi256(30, 31, 34);
        let panel = rgb_to_ansi256(43, 45, 48);
        let selection = rgb_to_ansi256(46, 67, 110);
        assert_ne!(canvas, selection);
        assert_ne!(panel, selection);
        assert!(
            canvas >= 232 || canvas == 16,
            "canvas maps to a dark gray: {canvas}"
        );
    }

    #[test]
    fn hsb_round_trip_matches_java() {
        assert_eq!(hsb_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsb_to_rgb(1.0 / 3.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsb_to_rgb(0.5, 0.0, 0.5), (128, 128, 128));
        let (h, s, b) = rgb_to_hsb(0, 0, 255);
        assert!((h - 2.0 / 3.0).abs() < 1e-6 && s == 1.0 && b == 1.0);
    }

    #[test]
    fn contrast_reference_values() {
        assert!((contrast_ratio((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast_ratio((30, 31, 34), (30, 31, 34)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn truecolor_detection_rules() {
        assert!(truecolor_from_env(Some("truecolor"), false, None));
        assert!(truecolor_from_env(Some("24bit"), false, None));
        assert!(truecolor_from_env(None, true, None));
        assert!(truecolor_from_env(None, false, Some("WezTerm")));
        assert!(!truecolor_from_env(None, false, None));
        assert!(!truecolor_from_env(Some("xterm-256color"), false, Some("Apple_Terminal")));
    }
}
