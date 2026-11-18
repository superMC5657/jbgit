use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    Unicode,
    NerdFont,
}

impl GlyphMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Unicode => Self::NerdFont,
            Self::NerdFont => Self::Unicode,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Unicode => "Unicode",
            Self::NerdFont => "NerdFont",
        }
    }

    pub const fn branch_icon(self) -> &'static str {
        match self {
            Self::Unicode => "⎇",
            Self::NerdFont => "\u{e0a0}",
        }
    }

    pub const fn head_icon(self) -> &'static str {
        match self {
            Self::Unicode => "◆",
            Self::NerdFont => "\u{f418}",
        }
    }

    pub const fn tag_icon(self) -> &'static str {
        match self {
            Self::Unicode => "⌗",
            Self::NerdFont => "\u{f02b}",
        }
    }

    pub const fn remote_icon(self) -> &'static str {
        match self {
            Self::Unicode => "☁",
            Self::NerdFont => "\u{f0c2}",
        }
    }

    pub const fn warn_icon(self) -> &'static str {
        match self {
            Self::Unicode => "⚠",
            Self::NerdFont => "\u{f071}",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThemePalette {
    #[allow(dead_code)]
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
    pub ref_head_fg: Color,
    pub ref_head_bg: Color,
    pub ref_local_fg: Color,
    pub ref_local_bg: Color,
    pub ref_remote_fg: Color,
    pub ref_remote_bg: Color,
    pub ref_tag_fg: Color,
    pub ref_tag_bg: Color,
    pub track_colors: [Color; 14],
}

impl ThemePalette {
    pub fn detect() -> Self {
        let true_color = detect_truecolor();
        Self::darcula(true_color)
    }

    pub fn darcula(true_color: bool) -> Self {
        let c = |r: u8, g: u8, b: u8| -> Color {
            if true_color {
                Color::Rgb(r, g, b)
            } else {
                Color::Indexed(rgb_to_ansi256(r, g, b))
            }
        };

        Self {
            true_color,
            bg_canvas: c(30, 31, 34),             // #1E1F22
            bg_panel: c(43, 45, 48),              // #2B2D30
            bg_elevated: c(49, 52, 56),           // #313438
            bg_selection_focused: c(46, 67, 110), // #2E436E
            bg_selection_unfocused: c(57, 59, 64),// #393B40
            bg_current_branch_row: c(37, 41, 48), // #252930
            bg_hover: c(45, 47, 52),              // #2D2F34
            border_splitter: c(57, 59, 64),       // #393B40
            border_focused: c(53, 116, 240),      // #3574F0
            fg_primary: c(188, 190, 196),         // #BCBEC4
            fg_bright: c(223, 225, 229),          // #DFE1E5
            fg_muted: c(111, 115, 122),           // #6F737A
            fg_accent_link: c(84, 138, 247),      // #548AF7
            status_modified: c(104, 151, 187),    // #6897BB
            status_added: c(98, 151, 85),         // #629755
            status_untracked: c(209, 103, 90),    // #D1675A
            status_deleted: c(128, 128, 128),     // #808080
            status_renamed: c(112, 174, 255),     // #70AEFF
            status_conflicted: c(229, 87, 101),   // #E55765
            ref_head_fg: c(242, 228, 155),        // #F2E49B
            ref_head_bg: c(58, 55, 40),           // #3A3728
            ref_local_fg: c(212, 232, 217),       // #D4E8D9
            ref_local_bg: c(40, 59, 46),          // #283B2E
            ref_remote_fg: c(223, 206, 240),      // #DFCEF0
            ref_remote_bg: c(54, 45, 62),         // #362D3E
            ref_tag_fg: c(184, 186, 192),         // #B8BAC0
            ref_tag_bg: c(49, 51, 55),            // #313337
            track_colors: [
                c(255, 98, 133),  // 0 Rose
                c(87, 209, 133),  // 1 Sage
                c(123, 111, 246), // 2 Indigo
                c(242, 177, 129), // 3 Apricot
                c(50, 224, 217),  // 4 Cyan
                c(254, 210, 119), // 5 Gold
                c(244, 107, 255), // 6 Magenta
                c(206, 158, 122), // 7 Bronze
                c(79, 242, 183),  // 8 Mint
                c(161, 119, 244), // 9 Violet
                c(70, 194, 199),  // 10 Ocean
                c(201, 133, 201), // 11 Plum
                c(236, 202, 136), // 12 Wheat
                c(184, 189, 201), // 13 Slate
            ],
        }
    }
}

fn detect_truecolor() -> bool {
    if let Ok(ct) = std::env::var("COLORTERM") {
        let low = ct.to_ascii_lowercase();
        if low.contains("truecolor") || low.contains("24bit") {
            return true;
        }
    }
    std::env::var_os("WT_SESSION").is_some()
        || std::env::var_os("TERM_PROGRAM")
            .and_then(|v| v.into_string().ok())
            .is_some_and(|s| matches!(s.as_str(), "WezTerm" | "iTerm.app" | "vscode" | "Alacritty"))
}

pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return (((r as u16 - 8) * 24) / 247) as u8 + 232;
    }
    let to_cube = |v: u8| -> u8 { ((v as u16 * 5 + 127) / 255) as u8 };
    16 + 36 * to_cube(r) + 6 * to_cube(g) + to_cube(b)
}
