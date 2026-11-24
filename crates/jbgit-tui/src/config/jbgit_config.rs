use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::keymap_config::KeymapPreset;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThemeFlavor {
    #[default]
    Darcula,
    CatppuccinMocha,
    GruvboxDark,
    Custom(String),
}

impl ThemeFlavor {
    pub fn parse(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "" | "darcula" => Self::Darcula,
            "catppuccin" | "catppuccin-mocha" | "catppuccin_mocha" => Self::CatppuccinMocha,
            "gruvbox" | "gruvbox-dark" | "gruvbox_dark" => Self::GruvboxDark,
            _ => Self::Custom(trimmed.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Darcula => "darcula",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::GruvboxDark => "gruvbox-dark",
            Self::Custom(s) => s.as_str(),
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
        Ok(Self::parse(&s))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusScanModePref {
    #[default]
    Auto,
    Always,
    Never,
}

impl StatusScanModePref {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "always" | "true" | "on" => Some(Self::Always),
            "never" | "false" | "off" => Some(Self::Never),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub const fn to_engine(self) -> jbgit_engine::StatusScanMode {
        match self {
            Self::Auto => jbgit_engine::StatusScanMode::Auto,
            Self::Always => jbgit_engine::StatusScanMode::Always,
            Self::Never => jbgit_engine::StatusScanMode::Never,
        }
    }
}

impl Serialize for StatusScanModePref {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StatusScanModePref {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::parse(&s).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PerformanceSection {
    #[serde(default = "default_large_index_entries")]
    pub large_index_entries: usize,
    #[serde(default)]
    pub worktree_renames: StatusScanModePref,
    #[serde(default = "default_recurse_untracked")]
    pub recurse_untracked_dirs: StatusScanModePref,
    #[serde(default = "default_true")]
    pub load_commit_bodies: bool,
}

fn default_large_index_entries() -> usize {
    jbgit_engine::pool::LARGE_INDEX_ENTRIES
}

fn default_recurse_untracked() -> StatusScanModePref {
    StatusScanModePref::Always
}

impl Default for PerformanceSection {
    fn default() -> Self {
        Self {
            large_index_entries: default_large_index_entries(),
            worktree_renames: StatusScanModePref::Auto,
            recurse_untracked_dirs: StatusScanModePref::Always,
            load_commit_bodies: true,
        }
    }
}

impl PerformanceSection {
    pub fn to_engine_config(&self) -> jbgit_engine::StatusScanConfig {
        jbgit_engine::StatusScanConfig {
            large_index_entries: self.large_index_entries,
            worktree_renames: self.worktree_renames.to_engine(),
            recurse_untracked_dirs: self.recurse_untracked_dirs.to_engine(),
            load_commit_bodies: self.load_commit_bodies,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotateSection {
    #[serde(default)]
    pub ignore_whitespace: bool,
    #[serde(default)]
    pub detect_moves: bool,
    #[serde(default = "default_annotate_gutter_width")]
    pub gutter_width: usize,
}

fn default_annotate_gutter_width() -> usize {
    28
}

impl Default for AnnotateSection {
    fn default() -> Self {
        Self {
            ignore_whitespace: false,
            detect_moves: false,
            gutter_width: default_annotate_gutter_width(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushSection {
    #[serde(default = "default_protected_branches")]
    pub protected_branches: Vec<String>,
}

fn default_protected_branches() -> Vec<String> {
    vec!["main".to_string(), "master".to_string()]
}

impl Default for PushSection {
    fn default() -> Self {
        Self {
            protected_branches: default_protected_branches(),
        }
    }
}

/// `[worktree]` — defaults of New Worktree (Spec 12 Issue 10).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeSection {
    /// Directory new working trees are created in (`<dir>/<repo>-<branch>`);
    /// unset / empty = the main working tree's parent directory. `~/` expands.
    #[serde(default)]
    pub default_parent: Option<String>,
}

/// `[update] mode` — how Update Project integrates upstream changes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateModePref {
    /// Follow `branch.<name>.rebase` / `pull.rebase`.
    #[default]
    GitConfig,
    Merge,
    Rebase,
}

impl UpdateModePref {
    pub fn to_update_mode(self) -> Option<jbgit_core::UpdateMode> {
        match self {
            Self::GitConfig => None,
            Self::Merge => Some(jbgit_core::UpdateMode::Merge),
            Self::Rebase => Some(jbgit_core::UpdateMode::Rebase),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateSection {
    #[serde(default)]
    pub mode: UpdateModePref,
    /// Stash local changes before updating and restore them afterwards.
    #[serde(default = "default_true")]
    pub save_changes: bool,
    /// Fast-forward other local branches that track an upstream.
    #[serde(default = "default_true")]
    pub update_other_branches: bool,
}

impl Default for UpdateSection {
    fn default() -> Self {
        Self {
            mode: UpdateModePref::GitConfig,
            save_changes: true,
            update_other_branches: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeymapSection {
    #[serde(default)]
    pub preset: KeymapPreset,
    #[serde(default = "default_footer_max_lines")]
    pub footer_max_lines: u16,
    #[serde(default)]
    pub tree_speed_search: Option<bool>,
}

fn default_footer_max_lines() -> u16 {
    1
}

impl Default for KeymapSection {
    fn default() -> Self {
        Self {
            preset: KeymapPreset::default(),
            footer_max_lines: default_footer_max_lines(),
            tree_speed_search: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JbgitConfig {
    #[serde(default)]
    pub general: GeneralSection,
    #[serde(default)]
    pub keymap: KeymapSection,
    #[serde(default)]
    pub graph: GraphSection,
    #[serde(default)]
    pub diff: DiffSection,
    #[serde(default)]
    pub annotate: AnnotateSection,
    #[serde(default)]
    pub push: PushSection,
    #[serde(default)]
    pub update: UpdateSection,
    #[serde(default)]
    pub worktree: WorktreeSection,
    #[serde(default)]
    pub layout: LayoutSection,
    #[serde(default)]
    pub performance: PerformanceSection,
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
    #[serde(default = "default_pair::<3, 2>")]
    pub log_details_ratio: PairRatio,
    #[serde(default = "default_pair::<2, 1>")]
    pub log_diff_right_ratio: PairRatio,
    #[serde(default = "default_pair::<3, 2>")]
    pub log_diff_bottom_ratio: PairRatio,
    #[serde(default = "default_pair::<1, 2>")]
    pub commit_ratio: PairRatio,
    #[serde(default = "default_pair::<3, 2>")]
    pub commit_left_ratio: PairRatio,
    #[serde(default = "default_pair::<2, 3>")]
    pub stash_ratio: PairRatio,
    #[serde(default = "default_pair::<1, 2>")]
    pub stash_preview_ratio: PairRatio,
    #[serde(default = "default_pair::<2, 3>")]
    pub console_ratio: PairRatio,
    #[serde(default = "default_pair::<2, 3>")]
    pub file_history_ratio: PairRatio,
    #[serde(default = "default_pair::<2, 3>")]
    pub file_history_vertical_ratio: PairRatio,
}

fn default_log_ratio() -> TripleRatio {
    TripleRatio([1, 3, 2])
}

fn default_pair<const A: u32, const B: u32>() -> PairRatio {
    PairRatio([A, B])
}

impl Default for LayoutSection {
    fn default() -> Self {
        Self {
            log_ratio: default_log_ratio(),
            log_details_ratio: default_pair::<3, 2>(),
            log_diff_right_ratio: default_pair::<2, 1>(),
            log_diff_bottom_ratio: default_pair::<3, 2>(),
            commit_ratio: default_pair::<1, 2>(),
            commit_left_ratio: default_pair::<3, 2>(),
            stash_ratio: default_pair::<2, 3>(),
            stash_preview_ratio: default_pair::<1, 2>(),
            console_ratio: default_pair::<2, 3>(),
            file_history_ratio: default_pair::<2, 3>(),
            file_history_vertical_ratio: default_pair::<2, 3>(),
        }
    }
}

impl LayoutSection {
    pub fn splitter_defaults(&self) -> [(jbgit_core::SplitterId, f32); 12] {
        let (log_left, log_right) = self.log_ratio.split_ratios();
        [
            (jbgit_core::SplitterId::LogLeftHorizontal, log_left),
            (jbgit_core::SplitterId::LogRightHorizontal, log_right),
            (
                jbgit_core::SplitterId::LogDetailsVertical,
                self.log_details_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::LogDiffRightHorizontal,
                self.log_diff_right_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::LogDiffBottomVertical,
                self.log_diff_bottom_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::CommitMainHorizontal,
                self.commit_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::CommitLeftVertical,
                self.commit_left_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::StashMainHorizontal,
                self.stash_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::StashPreviewVertical,
                self.stash_preview_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::ConsoleMainHorizontal,
                self.console_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::FileHistoryHorizontal,
                self.file_history_ratio.first_ratio(),
            ),
            (
                jbgit_core::SplitterId::FileHistoryVertical,
                self.file_history_vertical_ratio.first_ratio(),
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
            if let Some(flv) = &general_sec.flavor {
                self.general.flavor = flv.clone();
            }
        }
        if let Some(km) = &overlay.keymap {
            if let Some(p) = &km.preset {
                self.keymap.preset = p.clone();
            }
            if let Some(lines) = km.footer_max_lines {
                self.keymap.footer_max_lines = lines;
            }
            if let Some(tss) = km.tree_speed_search {
                self.keymap.tree_speed_search = Some(tss);
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
        if let Some(ann) = &overlay.annotate {
            if let Some(iw) = ann.ignore_whitespace {
                self.annotate.ignore_whitespace = iw;
            }
            if let Some(dm) = ann.detect_moves {
                self.annotate.detect_moves = dm;
            }
            if let Some(gw) = ann.gutter_width {
                self.annotate.gutter_width = gw.clamp(16, 60);
            }
        }
        if let Some(push) = &overlay.push
            && let Some(pb) = &push.protected_branches
        {
            self.push.protected_branches = pb.clone();
        }
        if let Some(wt) = &overlay.worktree
            && let Some(dp) = &wt.default_parent
        {
            self.worktree.default_parent = Some(dp.clone()).filter(|p| !p.trim().is_empty());
        }
        if let Some(up) = &overlay.update {
            if let Some(m) = up.mode {
                self.update.mode = m;
            }
            if let Some(s) = up.save_changes {
                self.update.save_changes = s;
            }
            if let Some(o) = up.update_other_branches {
                self.update.update_other_branches = o;
            }
        }
        if let Some(lo) = &overlay.layout {
            if let Some(r) = lo.log_ratio {
                self.layout.log_ratio = r;
            }
            for (dst, src) in [
                (&mut self.layout.log_details_ratio, lo.log_details_ratio),
                (
                    &mut self.layout.log_diff_right_ratio,
                    lo.log_diff_right_ratio,
                ),
                (
                    &mut self.layout.log_diff_bottom_ratio,
                    lo.log_diff_bottom_ratio,
                ),
                (&mut self.layout.commit_ratio, lo.commit_ratio),
                (&mut self.layout.commit_left_ratio, lo.commit_left_ratio),
                (&mut self.layout.stash_ratio, lo.stash_ratio),
                (
                    &mut self.layout.stash_preview_ratio,
                    lo.stash_preview_ratio,
                ),
                (&mut self.layout.console_ratio, lo.console_ratio),
                (&mut self.layout.file_history_ratio, lo.file_history_ratio),
                (
                    &mut self.layout.file_history_vertical_ratio,
                    lo.file_history_vertical_ratio,
                ),
            ] {
                if let Some(r) = src {
                    *dst = r;
                }
            }
        }
        if let Some(perf) = &overlay.performance {
            if let Some(n) = perf.large_index_entries {
                self.performance.large_index_entries = n;
            }
            if let Some(m) = perf.worktree_renames {
                self.performance.worktree_renames = m;
            }
            if let Some(m) = perf.recurse_untracked_dirs {
                self.performance.recurse_untracked_dirs = m;
            }
            if let Some(b) = perf.load_commit_bodies {
                self.performance.load_commit_bodies = b;
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
    pub keymap: Option<KeymapSectionOverlay>,
    pub graph: Option<GraphSectionOverlay>,
    pub diff: Option<DiffSectionOverlay>,
    pub annotate: Option<AnnotateSectionOverlay>,
    pub push: Option<PushSectionOverlay>,
    pub update: Option<UpdateSectionOverlay>,
    pub worktree: Option<WorktreeSectionOverlay>,
    pub layout: Option<LayoutSectionOverlay>,
    pub performance: Option<PerformanceSectionOverlay>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeymapSectionOverlay {
    pub preset: Option<KeymapPreset>,
    pub footer_max_lines: Option<u16>,
    pub tree_speed_search: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateSectionOverlay {
    pub mode: Option<UpdateModePref>,
    pub save_changes: Option<bool>,
    pub update_other_branches: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeSectionOverlay {
    pub default_parent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushSectionOverlay {
    pub protected_branches: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnnotateSectionOverlay {
    pub ignore_whitespace: Option<bool>,
    pub detect_moves: Option<bool>,
    pub gutter_width: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerformanceSectionOverlay {
    pub large_index_entries: Option<usize>,
    pub worktree_renames: Option<StatusScanModePref>,
    pub recurse_untracked_dirs: Option<StatusScanModePref>,
    pub load_commit_bodies: Option<bool>,
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
    pub log_details_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub log_diff_right_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub log_diff_bottom_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub commit_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub commit_left_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub stash_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub stash_preview_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub console_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub file_history_ratio: Option<PairRatio>,
    #[serde(default, deserialize_with = "deserialize_opt_pair_ratio")]
    pub file_history_vertical_ratio: Option<PairRatio>,
}
