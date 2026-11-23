//! Dual-mode glyph tables: plain Unicode (any monospace font) and Nerd Font.
//!
//! Every glyph here is exactly one terminal column wide (asserted in tests),
//! so layout code can measure them with `unicode-width` without per-mode cases.

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

    pub const fn symbols(self) -> &'static GlyphSymbols {
        match self {
            Self::Unicode => &UNICODE_SYMBOLS,
            Self::NerdFont => &NERD_FONT_SYMBOLS,
        }
    }
}

/// Graph track characters (Box Drawing / Geometric Shapes); shared by both modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphGlyphs {
    pub node: &'static str,
    pub node_head: &'static str,
    pub node_hollow: &'static str,
    pub vertical: &'static str,
    pub vertical_dashed: &'static str,
    pub arc_down_right: &'static str,
    pub arc_down_left: &'static str,
    pub arc_up_right: &'static str,
    pub arc_up_left: &'static str,
    pub horizontal: &'static str,
    pub tee_right: &'static str,
    pub tee_left: &'static str,
    pub cross: &'static str,
    pub arrow_down: &'static str,
    pub arrow_up: &'static str,
}

/// File-type icons for the changes tree and file lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileGlyphs {
    pub default_file: &'static str,
    pub rust: &'static str,
    pub code: &'static str,
    pub config: &'static str,
    pub docs: &'static str,
    pub folder: &'static str,
    pub submodule: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphSymbols {
    /// Pill end caps drawn with `fg = pill_bg, bg = row_bg`.
    pub pill_cap_left: &'static str,
    pub pill_cap_right: &'static str,
    pub head: &'static str,
    pub detached_head: &'static str,
    pub local_branch: &'static str,
    /// Second icon of a `TrackedPair` pill, drawn in the remote color.
    pub tracked_remote: &'static str,
    pub remote_branch: &'static str,
    pub tag: &'static str,
    pub warning: &'static str,
    pub favorite_star: &'static str,
    /// Colored bar used for collapsed refs in Compact References View.
    pub compact_bar: &'static str,
    pub arrow_up: &'static str,
    pub arrow_down: &'static str,
    pub tree_expanded: &'static str,
    pub tree_collapsed: &'static str,
    pub graph: GraphGlyphs,
    pub files: FileGlyphs,
}

const GRAPH_GLYPHS: GraphGlyphs = GraphGlyphs {
    node: "●",
    node_head: "◎",
    node_hollow: "○",
    vertical: "│",
    vertical_dashed: "┆",
    arc_down_right: "╭",
    arc_down_left: "╮",
    arc_up_right: "╰",
    arc_up_left: "╯",
    horizontal: "─",
    tee_right: "├",
    tee_left: "┤",
    cross: "┼",
    arrow_down: "▾",
    arrow_up: "▴",
};

const UNICODE_FILES: FileGlyphs = FileGlyphs {
    default_file: "▪",
    rust: "◇",
    code: "◈",
    config: "▫",
    docs: "≡",
    folder: "▸",
    submodule: "⊞",
};

const NERD_FONT_FILES: FileGlyphs = FileGlyphs {
    default_file: "\u{f15b}",
    rust: "\u{e7a8}",
    code: "\u{f121}",
    config: "\u{e615}",
    docs: "\u{f48a}",
    folder: "\u{f07b}",
    submodule: "\u{f1c0}",
};

pub const UNICODE_SYMBOLS: GlyphSymbols = GlyphSymbols {
    pill_cap_left: "▐",
    pill_cap_right: "▌",
    head: "◆",
    detached_head: "⚠",
    local_branch: "⎇",
    tracked_remote: "⇄",
    remote_branch: "☁",
    tag: "⌗",
    warning: "⚠",
    favorite_star: "★",
    compact_bar: "┃",
    arrow_up: "↑",
    arrow_down: "↓",
    tree_expanded: "▼",
    tree_collapsed: "▶",
    graph: GRAPH_GLYPHS,
    files: UNICODE_FILES,
};

pub const NERD_FONT_SYMBOLS: GlyphSymbols = GlyphSymbols {
    pill_cap_left: "\u{e0b6}",
    pill_cap_right: "\u{e0b4}",
    head: "\u{f418}",
    detached_head: "\u{f071}",
    local_branch: "\u{e0a0}",
    tracked_remote: "\u{f444}",
    remote_branch: "\u{f0c2}",
    tag: "\u{f02b}",
    warning: "\u{f071}",
    favorite_star: "\u{f005}",
    compact_bar: "┃",
    arrow_up: "↑",
    arrow_down: "↓",
    tree_expanded: "\u{f078}",
    tree_collapsed: "\u{f054}",
    graph: GRAPH_GLYPHS,
    files: NERD_FONT_FILES,
};

impl GlyphSymbols {
    /// Returns the file-type icon for a repository path.
    pub fn file_icon(&self, path: &str) -> &'static str {
        if path.ends_with('/') {
            return self.files.folder;
        }
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext.to_ascii_lowercase().as_str() {
            "rs" => self.files.rust,
            "toml" | "yaml" | "yml" | "json" | "lock" | "ini" | "conf" => self.files.config,
            "md" | "txt" | "rst" => self.files.docs,
            "kt" | "java" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "cpp" | "h" => {
                self.files.code
            }
            _ => self.files.default_file,
        }
    }

    /// Formats ahead/behind counts using this glyph set (`" ↑2 ↓1"`, `" ↑1"`, `""`).
    pub fn format_ahead_behind(&self, ahead_behind: Option<(usize, usize)>) -> String {
        let Some((ahead, behind)) = ahead_behind else {
            return String::new();
        };
        let mut out = String::new();
        if ahead > 0 {
            out.push_str(&format!(" {}{ahead}", self.arrow_up));
        }
        if behind > 0 {
            out.push_str(&format!(" {}{behind}", self.arrow_down));
        }
        out
    }

    /// Every glyph with its role name (used by the palette preview and tests).
    pub fn named(&self) -> Vec<(&'static str, &'static str)> {
        let g = &self.graph;
        let f = &self.files;
        vec![
            ("cap_l", self.pill_cap_left),
            ("cap_r", self.pill_cap_right),
            ("head", self.head),
            ("detached", self.detached_head),
            ("local", self.local_branch),
            ("tracked", self.tracked_remote),
            ("remote", self.remote_branch),
            ("tag", self.tag),
            ("warn", self.warning),
            ("star", self.favorite_star),
            ("bar", self.compact_bar),
            ("up", self.arrow_up),
            ("down", self.arrow_down),
            ("open", self.tree_expanded),
            ("closed", self.tree_collapsed),
            ("file", f.default_file),
            ("rust", f.rust),
            ("code", f.code),
            ("conf", f.config),
            ("docs", f.docs),
            ("dir", f.folder),
            ("submodule", f.submodule),
            ("node", g.node),
            ("head_node", g.node_head),
            ("hollow", g.node_hollow),
            ("vert", g.vertical),
            ("dashed", g.vertical_dashed),
            ("╭", g.arc_down_right),
            ("╮", g.arc_down_left),
            ("╰", g.arc_up_right),
            ("╯", g.arc_up_left),
            ("horiz", g.horizontal),
            ("├", g.tee_right),
            ("┤", g.tee_left),
            ("┼", g.cross),
            ("▾", g.arrow_down),
            ("▴", g.arrow_up),
        ]
    }
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    #[test]
    fn every_glyph_is_single_column_in_both_modes() {
        for mode in [GlyphMode::Unicode, GlyphMode::NerdFont] {
            for (name, glyph) in mode.symbols().named() {
                assert_eq!(
                    glyph.width(),
                    1,
                    "{mode:?} glyph `{name}` ({glyph:?}) must be 1 column"
                );
                assert_eq!(glyph.chars().count(), 1, "{mode:?} `{name}` is one scalar");
            }
        }
    }

    #[test]
    fn modes_differ_in_caps_and_toggle_round_trips() {
        assert_eq!(UNICODE_SYMBOLS.pill_cap_left, "▐");
        assert_eq!(UNICODE_SYMBOLS.pill_cap_right, "▌");
        assert_eq!(NERD_FONT_SYMBOLS.pill_cap_left, "\u{e0b6}");
        assert_eq!(NERD_FONT_SYMBOLS.pill_cap_right, "\u{e0b4}");
        assert_eq!(GlyphMode::Unicode.toggle().toggle(), GlyphMode::Unicode);
    }
}
