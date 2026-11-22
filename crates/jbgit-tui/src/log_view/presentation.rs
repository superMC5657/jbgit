#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationToggle {
    CompactRefs,
    ShowTagNames,
    ShowLongEdges,
    IntelliSort,
    FirstParentOnly,
    HighlightCurrentBranch,
    HighlightMerges,
    ShowHashColumn,
    RelativeDates,
    UseAuthorDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPresentation {
    /// Compact References View: only the first ref keeps its text.
    pub compact_refs: bool,
    /// `Show Tag Names`: when off, tags collapse to icons.
    pub show_tag_names: bool,
    pub highlight_current_branch: bool,
    pub highlight_merges: bool,
    pub show_hash_column: bool,
    pub relative_dates: bool,
    /// Show author date instead of commit date.
    pub use_author_date: bool,
}

impl Default for LogPresentation {
    fn default() -> Self {
        Self {
            compact_refs: false,
            show_tag_names: true,
            highlight_current_branch: true,
            highlight_merges: true,
            show_hash_column: false,
            relative_dates: true,
            use_author_date: false,
        }
    }
}

/// `2h ago`, `Yesterday`, `5d ago`, else `YYYY-MM-DD`.
pub fn format_relative_time(ts: i64, now: i64) -> String {
    let delta = now - ts;
    if delta < 0 {
        return format_absolute_time(ts, false);
    }
    match delta {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", delta / 60),
        3_600..86_400 => format!("{}h ago", delta / 3_600),
        86_400..172_800 => "Yesterday".to_string(),
        172_800..604_800 => format!("{}d ago", delta / 86_400),
        _ => format_absolute_time(ts, false),
    }
}

/// `2026-11-18 14:20` (or date only).
pub fn format_absolute_time(ts: i64, with_time: bool) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) if with_time => dt.format("%Y-%m-%d %H:%M").to_string(),
        Some(dt) => dt.format("%Y-%m-%d").to_string(),
        None => String::new(),
    }
}
