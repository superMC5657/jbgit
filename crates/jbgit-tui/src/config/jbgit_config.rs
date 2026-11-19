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
pub struct JbgitConfig {
    #[serde(default)]
    pub general: GeneralSection,
    #[serde(default)]
    pub graph: GraphSection,
    #[serde(default)]
    pub diff: DiffSection,
    #[serde(default)]
    pub layout: LayoutSection,
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

pub const RATIO_MIN: f32 = 0.05;
pub const RATIO_MAX: f32 = 0.90;

fn clamp_ratio(num: u32, denom: u64) -> f32 {
    ((num as f64) / (denom as f64)).clamp(RATIO_MIN as f64, RATIO_MAX as f64) as f32
}

/// Validated two-pane weight pair (sum must be > 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairRatio([u32; 2]);

impl PairRatio {
    pub const fn new(a: u32, b: u32) -> Option<Self> {
        if (a as u64) + (b as u64) > 0 {
            Some(Self([a, b]))
        } else {
            None
        }
    }

    pub fn from_slice(weights: &[u32]) -> Option<Self> {
        match weights {
            &[a, b] => Self::new(a, b),
            _ => None,
        }
    }

    pub const fn weights(self) -> [u32; 2] {
        self.0
    }

    pub fn first_ratio(self) -> f32 {
        clamp_ratio(self.0[0], (self.0[0] as u64) + (self.0[1] as u64))
    }
}

impl Serialize for PairRatio {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PairRatio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<u32>::deserialize(deserializer)?;
        Self::from_slice(&raw)
            .ok_or_else(|| serde::de::Error::custom("expected 2 non-negative weights with sum > 0"))
    }
}

/// Validated three-pane weight triple (middle + right sum must be > 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TripleRatio([u32; 3]);

impl TripleRatio {
    pub const fn new(a: u32, b: u32, c: u32) -> Option<Self> {
        if (b as u64) + (c as u64) > 0 {
            Some(Self([a, b, c]))
        } else {
            None
        }
    }

    pub fn from_slice(weights: &[u32]) -> Option<Self> {
        match weights {
            &[a, b, c] => Self::new(a, b, c),
            _ => None,
        }
    }

    pub const fn weights(self) -> [u32; 3] {
        self.0
    }

    pub fn split_ratios(self) -> (f32, f32) {
        let [a, b, c] = self.0;
        let bc = (b as u64) + (c as u64);
        let total = (a as u64) + bc;
        (clamp_ratio(a, total), clamp_ratio(b, bc))
    }
}

impl Serialize for TripleRatio {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TripleRatio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<u32>::deserialize(deserializer)?;
        Self::from_slice(&raw).ok_or_else(|| {
            serde::de::Error::custom("expected 3 non-negative weights with trailing pair sum > 0")
        })
    }
}

fn deserialize_opt_pair_ratio<'de, D>(deserializer: D) -> Result<Option<PairRatio>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<Vec<u32>>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(PairRatio::from_slice))
}

fn deserialize_opt_triple_ratio<'de, D>(deserializer: D) -> Result<Option<TripleRatio>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<Vec<u32>>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(TripleRatio::from_slice))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LayoutSection {
    #[serde(default = "default_log_ratio")]
    pub log_ratio: TripleRatio,
    #[serde(default = "default_commit_ratio")]
    pub commit_ratio: PairRatio,
    #[serde(default = "default_stash_ratio")]
    pub stash_ratio: PairRatio,
    #[serde(default = "default_console_ratio")]
    pub console_ratio: PairRatio,
}

fn default_log_ratio() -> TripleRatio {
    TripleRatio([1, 3, 2])
}

fn default_commit_ratio() -> PairRatio {
    PairRatio([1, 2])
}

fn default_stash_ratio() -> PairRatio {
    PairRatio([2, 3])
}

fn default_console_ratio() -> PairRatio {
    PairRatio([2, 3])
}

impl Default for LayoutSection {
    fn default() -> Self {
        Self {
            log_ratio: default_log_ratio(),
            commit_ratio: default_commit_ratio(),
            stash_ratio: default_stash_ratio(),
            console_ratio: default_console_ratio(),
        }
    }
}

impl LayoutSection {
    pub fn splitter_defaults(&self) -> [(jbgit_core::SplitterId, f32); 5] {
        let (log_left, log_right) = self.log_ratio.split_ratios();
        [
            (jbgit_core::SplitterId::LogLeftHorizontal, log_left),
            (jbgit_core::SplitterId::LogRightHorizontal, log_right),
            (
                jbgit_core::SplitterId::CommitMainHorizontal,
                self.commit_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::StashMainHorizontal,
                self.stash_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::ConsoleMainHorizontal,
                self.console_ratio.first_ratio(),
            ),
        ]
    }
}

impl JbgitConfig {
    /// Merge an optional partial override on top of self.
    pub fn merge_overlay(&mut self, overlay: &JbgitConfigOverlay) {
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
        if let Some(lo) = &overlay.layout {
            if let Some(r) = lo.log_ratio {
                self.layout.log_ratio = r;
            }
            if let Some(r) = lo.commit_ratio {
                self.layout.commit_ratio = r;
            }
            if let Some(r) = lo.stash_ratio {
                self.layout.stash_ratio = r;
            }
            if let Some(r) = lo.console_ratio {
                self.layout.console_ratio = r;
            }
        }
    }
}

fn g_mode(v: Option<GlyphModePref>) -> Option<GlyphModePref> {
    v
}

/// Partial overlay for parsing user config files that may omit sections/keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JbgitConfigOverlay {
    pub general: Option<GeneralSectionOverlay>,
    pub graph: Option<GraphSectionOverlay>,
    pub diff: Option<DiffSectionOverlay>,
    pub layout: Option<LayoutSectionOverlay>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutSectionOverlay {
    #[serde(default, deserialize_with = "deserialize_opt_triple_ratio")]
    pub log_ratio: Option<TripleRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub commit_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub stash_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub console_ratio: Option<PairRatio>,
}
