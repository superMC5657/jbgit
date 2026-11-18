use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LanguagePref {
    #[default]
    ZhCn,
    EnUs,
    Auto,
    Custom(String),
}

impl LanguagePref {
    pub fn parse(s: &str) -> Self {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("auto") {
            Self::Auto
        } else if trimmed.eq_ignore_ascii_case("zh-cn") || trimmed.eq_ignore_ascii_case("zh_cn") {
            Self::ZhCn
        } else if trimmed.eq_ignore_ascii_case("en-us") || trimmed.eq_ignore_ascii_case("en_us") {
            Self::EnUs
        } else {
            Self::Custom(trimmed.to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
            Self::Auto => "auto",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl Serialize for LanguagePref {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LanguagePref {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultTab {
    #[default]
    Log,
    Commit,
    Branches,
    Stash,
    Console,
}

impl DefaultTab {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "log" => Some(Self::Log),
            "commit" | "stage" => Some(Self::Commit),
            "branches" => Some(Self::Branches),
            "stash" => Some(Self::Stash),
            "console" => Some(Self::Console),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Commit => "commit",
            Self::Branches => "branches",
            Self::Stash => "stash",
            Self::Console => "console",
        }
    }
}

impl Serialize for DefaultTab {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DefaultTab {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeFlavor {
    #[default]
    Darcula,
    CatppuccinMocha,
    GruvboxDark,
}

impl ThemeFlavor {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "darcula" => Some(Self::Darcula),
            "catppuccin" | "catppuccin-mocha" | "catppuccin_mocha" => Some(Self::CatppuccinMocha),
            "gruvbox" | "gruvbox-dark" | "gruvbox_dark" => Some(Self::GruvboxDark),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Darcula => "darcula",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::GruvboxDark => "gruvbox-dark",
        }
    }
}

impl Serialize for ThemeFlavor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ThemeFlavor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphBackendPref {
    #[default]
    Auto,
    Vector,
    Cell,
}

impl GraphBackendPref {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "vector" => Some(Self::Vector),
            "cell" | "grid" | "character_grid" => Some(Self::Cell),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vector => "vector",
            Self::Cell => "cell",
        }
    }
}

impl Serialize for GraphBackendPref {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GraphBackendPref {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphModePref {
    #[default]
    NerdFont,
    Unicode,
}

impl GlyphModePref {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nerd_font" | "nerdfont" | "nerd-font" => Some(Self::NerdFont),
            "unicode" => Some(Self::Unicode),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NerdFont => "nerd_font",
            Self::Unicode => "unicode",
        }
    }
}

impl Serialize for GlyphModePref {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GlyphModePref {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JgitConfig {
    #[serde(default)]
    pub general: GeneralSection,
    #[serde(default)]
    pub graph: GraphSection,
    #[serde(default)]
    pub diff: DiffSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneralSection {
    #[serde(default)]
    pub language: LanguagePref,
    #[serde(default)]
    pub default_tab: DefaultTab,
    #[serde(default)]
    pub flavor: ThemeFlavor,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphSection {
    #[serde(default)]
    pub backend: GraphBackendPref,
    #[serde(default)]
    pub glyph_mode: GlyphModePref,
    #[serde(default)]
    pub compact_refs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSection {
    #[serde(default = "default_true")]
    pub syntax_highlight: bool,
    #[serde(default = "default_tab_size")]
    pub tab_size: usize,
    #[serde(default = "default_context_lines")]
    pub context_lines: usize,
}

fn default_true() -> bool {
    true
}

fn default_tab_size() -> usize {
    4
}

fn default_context_lines() -> usize {
    3
}

impl Default for DiffSection {
    fn default() -> Self {
        Self {
            syntax_highlight: true,
            tab_size: 4,
            context_lines: 3,
        }
    }
}

impl JgitConfig {
    /// Merge an optional partial override on top of self.
    pub fn merge_overlay(&mut self, overlay: &JgitConfigOverlay) {
        if let Some(general_sec) = &overlay.general {
            if let Some(lang) = &general_sec.language {
                self.general.language = lang.clone();
            }
            if let Some(tab) = general_sec.default_tab {
                self.general.default_tab = tab;
            }
            if let Some(flv) = general_sec.flavor {
                self.general.flavor = flv;
            }
        }
        if let Some(gr) = &overlay.graph {
            if let Some(b) = gr.backend {
                self.graph.backend = b;
            }
            if let Some(g) = g_mode(gr.glyph_mode) {
                self.graph.glyph_mode = g;
            }
            if let Some(c) = gr.compact_refs {
                self.graph.compact_refs = c;
            }
        }
        if let Some(df) = &overlay.diff {
            if let Some(sh) = df.syntax_highlight {
                self.diff.syntax_highlight = sh;
            }
            if let Some(ts) = df.tab_size {
                self.diff.tab_size = ts.max(1);
            }
            if let Some(cl) = df.context_lines {
                self.diff.context_lines = cl;
            }
        }
    }
}

fn g_mode(v: Option<GlyphModePref>) -> Option<GlyphModePref> {
    v
}

/// Partial overlay for parsing user config files that may omit sections/keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JgitConfigOverlay {
    pub general: Option<GeneralSectionOverlay>,
    pub graph: Option<GraphSectionOverlay>,
    pub diff: Option<DiffSectionOverlay>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneralSectionOverlay {
    pub language: Option<LanguagePref>,
    pub default_tab: Option<DefaultTab>,
    pub flavor: Option<ThemeFlavor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSectionOverlay {
    pub backend: Option<GraphBackendPref>,
    pub glyph_mode: Option<GlyphModePref>,
    pub compact_refs: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffSectionOverlay {
    pub syntax_highlight: Option<bool>,
    pub tab_size: Option<usize>,
    pub context_lines: Option<usize>,
}
