//! UI state for the `[2 Log]` three-pane workbench that is not graph
//! projection: Branches Dashboard, filter bar, presentation settings and the
//! right-hand changes / details / diff panes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jgit_core::{
    BranchFilter, BranchTreeNode, BranchTreeRow, BranchesPanelSettings, BranchesTreeModel,
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, DateFilter, DetailsKey, DiffContext,
    FileChange, FileDiff, FileDiffKey, LogFilterSpec, LruCache, Oid, PathFilter, RepoSnapshot,
    UserFilter, build_change_rows,
};
use rustc_hash::FxHashSet;

use crate::syntax::HighlightedDiff;

/// Terminal width below which the Branches pane auto-collapses.
pub const NARROW_WIDTH: u16 = 110;
/// Debounce between a selection change and the details request.
pub const DETAILS_DEBOUNCE: Duration = Duration::from_millis(30);
pub const DETAILS_CACHE_CAPACITY: usize = 256;
pub const DIFF_CACHE_CAPACITY: usize = 64;

// ───────────────────────────── Branches pane ─────────────────────────────

#[derive(Debug, Default)]
pub struct BranchesPaneState {
    pub settings: BranchesPanelSettings,
    pub model: BranchesTreeModel,
    pub rows: Vec<BranchTreeRow>,
    pub selected: usize,
    /// Node keys picked with Ctrl+Click for a union branch filter.
    pub multi: Vec<String>,
    /// Inline speed-search query.
    pub query: String,
    /// `Alt+B` / `[«]` collapse on a wide terminal.
    pub hidden_by_user: bool,
    /// `Alt+B` on a narrow terminal re-opens the auto-collapsed pane.
    pub forced_open: bool,
    pub settings_path: Option<PathBuf>,
    source: Option<Arc<RepoSnapshot>>,
}

impl BranchesPaneState {
    pub fn is_collapsed(&self, total_width: u16) -> bool {
        if total_width < NARROW_WIDTH {
            !self.forced_open
        } else {
            self.hidden_by_user
        }
    }

    pub fn toggle_collapsed(&mut self, total_width: u16) {
        if total_width < NARROW_WIDTH {
            self.forced_open = !self.forced_open;
        } else {
            self.hidden_by_user = !self.hidden_by_user;
        }
    }

    pub fn load_settings(&mut self, path: PathBuf) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            self.settings = BranchesPanelSettings::parse(&text);
        }
        self.settings_path = Some(path);
        self.source = None;
    }

    pub fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            let _ = std::fs::write(path, self.settings.serialize());
        }
    }

    /// Rebuilds the tree model when the snapshot changed (or `force`).
    pub fn sync_model(&mut self, snap: Option<&Arc<RepoSnapshot>>, force: bool) {
        let stale = match (snap, &self.source) {
            (Some(s), Some(src)) => !Arc::ptr_eq(s, src),
            (None, None) => false,
            _ => true,
        };
        if !stale && !force {
            return;
        }
        let selected_key = self.selected_node().map(|n| n.key.clone());
        self.model = match snap {
            Some(s) => BranchesTreeModel::build(
                &s.refs,
                s.head_branch.as_deref(),
                s.head_oid,
                &self.settings.favorites,
                self.settings.options,
            ),
            None => BranchesTreeModel::default(),
        };
        self.source = snap.cloned();
        let valid: FxHashSet<&str> = self.model.nodes.iter().map(|n| n.key.as_str()).collect();
        self.multi.retain(|k| valid.contains(k.as_str()));
        self.refresh_rows();
        if let Some(key) = selected_key {
            self.select_key(&key);
        }
    }

    pub fn refresh_rows(&mut self) {
        self.rows = self
            .model
            .visible_rows(&self.settings.expansion, &self.query);
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn select_key(&mut self, key: &str) {
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| self.model.nodes[r.node].key == key)
        {
            self.selected = pos;
        }
    }

    pub fn selected_row(&self) -> Option<BranchTreeRow> {
        self.rows.get(self.selected).copied()
    }

    pub fn selected_node(&self) -> Option<&BranchTreeNode> {
        self.selected_row().map(|r| &self.model.nodes[r.node])
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    /// Ref names for the branch filter: the Ctrl+Click set if any, otherwise
    /// the selected node (folders expand to every child branch).
    pub fn filter_ref_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let push_node = |idx: usize, out: &mut Vec<String>| {
            for n in self.model.collect_ref_names(idx) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        };
        if self.multi.is_empty() {
            if let Some(row) = self.selected_row() {
                push_node(row.node, &mut out);
            }
        } else {
            for key in &self.multi {
                if let Some(idx) = self.model.find_by_key(key) {
                    push_node(idx, &mut out);
                }
            }
        }
        out
    }

    pub fn toggle_multi(&mut self, key: &str) {
        if let Some(pos) = self.multi.iter().position(|k| k == key) {
            self.multi.remove(pos);
        } else {
            self.multi.push(key.to_string());
        }
    }
}

// ───────────────────────────── Filter bar ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterField {
    Branch,
    User,
    Date,
    Paths,
}

impl FilterField {
    pub const ALL: [Self; 4] = [Self::Branch, Self::User, Self::Date, Self::Paths];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Branch => "Branch",
            Self::User => "User",
            Self::Date => "Date",
            Self::Paths => "Paths",
        }
    }

    pub const fn hint(self) -> &'static str {
        match self {
            Self::Branch => "names / globs, -exclude   e.g. main feat/* -dependabot/*",
            Self::User => "@me, names or emails      e.g. @me alice",
            Self::Date => "today | yesterday | 7d | 30d | YYYY-MM-DD..YYYY-MM-DD",
            Self::Paths => "repo-relative dirs / files  e.g. src/graph docs",
        }
    }

    pub fn label(self, spec: &LogFilterSpec) -> String {
        match self {
            Self::Branch => spec.branch.label(),
            Self::User => spec.user.label(),
            Self::Date => {
                if spec.date.is_empty() {
                    "All".to_string()
                } else {
                    spec.date.label.clone()
                }
            }
            Self::Paths => spec.paths.label(),
        }
    }

    pub fn expr(self, spec: &LogFilterSpec) -> String {
        match self {
            Self::Branch => spec.branch.to_expr(),
            Self::User => spec.user.users.join(" "),
            Self::Date => spec.date.label.clone(),
            Self::Paths => spec.paths.paths.join(" "),
        }
    }

    pub fn is_active(self, spec: &LogFilterSpec) -> bool {
        match self {
            Self::Branch => !spec.branch.is_empty(),
            Self::User => !spec.user.is_empty(),
            Self::Date => !spec.date.is_empty(),
            Self::Paths => !spec.paths.is_empty(),
        }
    }

    pub fn clear(self, spec: &mut LogFilterSpec) {
        match self {
            Self::Branch => spec.branch = BranchFilter::default(),
            Self::User => spec.user = UserFilter::default(),
            Self::Date => spec.date = DateFilter::default(),
            Self::Paths => spec.paths = PathFilter::default(),
        }
    }

    pub fn apply(self, spec: &mut LogFilterSpec, expr: &str, now: i64) -> Result<(), String> {
        match self {
            Self::Branch => spec.branch = BranchFilter::parse(expr),
            Self::User => spec.user = UserFilter::parse(expr),
            Self::Date => {
                spec.date = DateFilter::parse(expr, now)
                    .ok_or_else(|| format!("Date filter: cannot parse '{}'", expr.trim()))?;
            }
            Self::Paths => spec.paths = PathFilter::parse(expr),
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PathRequest {
    pub paths: Vec<String>,
    pub snapshot: Arc<RepoSnapshot>,
}

#[derive(Debug, Clone)]
pub struct PathMatches {
    pub paths: Vec<String>,
    pub snapshot: Arc<RepoSnapshot>,
    pub oids: Arc<HashSet<Oid>>,
}

#[derive(Debug, Default)]
pub struct FilterBarState {
    pub spec: LogFilterSpec,
    /// Text box has keyboard focus (`Ctrl+L` / `/`).
    pub editing_text: bool,
    /// Path matches for `spec.paths` (paths they were computed for + set).
    pub path_matches: Option<PathMatches>,
    /// Paths + snapshot of the in-flight path request.
    pub path_requested: Option<PathRequest>,
}

impl FilterBarState {
    pub fn field_label(&self, field: FilterField) -> String {
        field.label(&self.spec)
    }

    pub fn field_expr(&self, field: FilterField) -> String {
        field.expr(&self.spec)
    }

    pub fn field_active(&self, field: FilterField) -> bool {
        field.is_active(&self.spec)
    }

    /// Path matches valid for the current paths and snapshot.
    pub fn current_path_matches(&self, snap: Option<&Arc<RepoSnapshot>>) -> Option<&HashSet<Oid>> {
        let pm = self.path_matches.as_ref()?;
        let snap = snap?;
        (pm.paths == self.spec.paths.paths && Arc::ptr_eq(&pm.snapshot, snap))
            .then_some(pm.oids.as_ref())
    }

    pub fn path_matches_pending(&self, snap: Option<&Arc<RepoSnapshot>>) -> bool {
        !self.spec.paths.is_empty() && self.current_path_matches(snap).is_none()
    }
}

// ───────────────────────────── Presentation ─────────────────────────────

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
            highlight_current_branch: true,
            highlight_merges: true,
            show_hash_column: false,
            relative_dates: true,
            use_author_date: false,
        }
    }
}

// ───────────────────────────── Right panes ─────────────────────────────

/// Inline diff preview dock location (`MoveDiffPreviewToRight` / `MoveDiffPreviewToBottom`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffPreviewDock {
    /// Replace the bottom-right Commit Details panel.
    #[default]
    Details,
    /// Expand a 4th column on the far right of the Log tab.
    Right,
    /// Expand a full-width bar at the bottom of the Log tab.
    Bottom,
}

impl DiffPreviewDock {
    pub const fn next(self) -> Self {
        match self {
            Self::Details => Self::Right,
            Self::Right => Self::Bottom,
            Self::Bottom => Self::Details,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Details => "Details",
            Self::Right => "Right",
            Self::Bottom => "Bottom",
        }
    }
}

#[derive(Debug)]
pub struct DetailsPaneState {
    pub cache: LruCache<DetailsKey, Arc<CommitChangesBundle>>,
    pub diff_cache: LruCache<FileDiffKey, Arc<FileDiff>>,
    pub diff_syntax: LruCache<FileDiffKey, Arc<HighlightedDiff>>,
    /// Details the right panes currently represent.
    pub current_key: Option<DetailsKey>,
    /// Explicit comparison override (`Compare Versions` / `Compare with Local`).
    pub compare_override: Option<DetailsKey>,
    /// Debounced request: key + deadline.
    pub pending: Option<(DetailsKey, Instant)>,
    /// Which parent a single merge commit is compared against.
    pub merge_parent: usize,
    pub requested_diff: Option<FileDiffKey>,
    /// Containing branches/tags for the single selected commit.
    pub containing: Option<(Oid, Vec<String>)>,

    pub changes_selected: usize,
    pub changes_group_by_dir: bool,
    pub changes_collapsed: FxHashSet<String>,

    pub details_scroll: usize,
    /// Inline diff preview visibility.
    pub diff_preview: bool,
    /// Where the inline diff preview is docked (`Details` / `Right` / `Bottom`).
    pub diff_dock: DiffPreviewDock,
    /// Full-screen zoomed diff (`F4` / `Ctrl+Enter`).
    pub diff_zoomed: bool,
    pub diff_side_by_side: bool,
    pub diff_context: DiffContext,
    pub diff_scroll: usize,
}

impl Default for DetailsPaneState {
    fn default() -> Self {
        Self {
            cache: LruCache::new(DETAILS_CACHE_CAPACITY),
            diff_cache: LruCache::new(DIFF_CACHE_CAPACITY),
            diff_syntax: LruCache::new(DIFF_CACHE_CAPACITY),
            current_key: None,
            compare_override: None,
            pending: None,
            merge_parent: 0,
            requested_diff: None,
            containing: None,
            changes_selected: 0,
            changes_group_by_dir: true,
            changes_collapsed: FxHashSet::default(),
            details_scroll: 0,
            diff_preview: false,
            diff_dock: DiffPreviewDock::Details,
            diff_zoomed: false,
            diff_side_by_side: false,
            diff_context: DiffContext::Three,
            diff_scroll: 0,
        }
    }
}

impl DetailsPaneState {
    pub fn current_bundle(&self) -> Option<&Arc<CommitChangesBundle>> {
        self.cache.peek(self.current_key.as_ref()?)
    }

    pub fn change_rows(&self) -> Vec<ChangeTreeRow> {
        self.current_bundle().map_or_else(Vec::new, |b| {
            build_change_rows(
                &b.changes,
                self.changes_group_by_dir,
                &self.changes_collapsed,
            )
        })
    }

    /// Selected changed file, if the selected row is a file.
    pub fn selected_change(&self) -> Option<&FileChange> {
        let bundle = self.current_bundle()?;
        let rows = self.change_rows();
        match rows.get(self.changes_selected)?.kind {
            ChangeRowKind::File { change_idx } => bundle.changes.get(change_idx),
            ChangeRowKind::Dir { .. } => None,
        }
    }

    /// File-row positions (indices into `change_rows`) for `[` / `]` paging.
    pub fn file_row_positions(&self) -> Vec<usize> {
        self.change_rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, ChangeRowKind::File { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn desired_diff_key(&self) -> Option<FileDiffKey> {
        let details = self.current_key.clone()?;
        let change = self.selected_change()?;
        Some(FileDiffKey {
            details,
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            context: self.diff_context,
        })
    }

    pub fn current_diff(&self) -> Option<&Arc<FileDiff>> {
        self.diff_cache.peek(&self.desired_diff_key()?)
    }

    pub fn current_diff_syntax(&self) -> Option<&Arc<HighlightedDiff>> {
        self.diff_syntax.peek(&self.desired_diff_key()?)
    }

    pub fn diff_visible(&self) -> bool {
        self.diff_preview || self.diff_zoomed
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
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

/// Suggestions for a filter field editor, fuzzy-filtered by the last token.
pub fn filter_suggestions(
    field: FilterField,
    snap: Option<&RepoSnapshot>,
    query: &str,
) -> Vec<String> {
    let last = query
        .rsplit(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("")
        .trim_start_matches(['-', '+']);
    let mut base: Vec<String> = match field {
        FilterField::Branch => {
            let mut v = vec!["HEAD".to_string()];
            if let Some(s) = snap {
                v.extend(s.refs.iter().map(|r| r.name.clone()));
            }
            v
        }
        FilterField::User => {
            let mut v = vec!["@me".to_string()];
            if let Some(s) = snap {
                let mut counts: rustc_hash::FxHashMap<&str, usize> = Default::default();
                for c in &s.commits {
                    *counts.entry(c.author_name.as_str()).or_default() += 1;
                }
                let mut authors: Vec<_> = counts.into_iter().collect();
                authors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                v.extend(authors.into_iter().take(200).map(|(a, _)| a.to_string()));
            }
            v
        }
        FilterField::Date => ["today", "yesterday", "7d", "30d", "all"]
            .iter()
            .map(ToString::to_string)
            .collect(),
        FilterField::Paths => {
            let mut v: Vec<String> = Vec::new();
            if let Some(s) = snap
                && let Ok(entries) = std::fs::read_dir(&s.workdir)
            {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !name.starts_with('.') {
                        v.push(name);
                    }
                }
                v.sort();
            }
            v
        }
    };
    base.dedup();
    if last.is_empty() {
        return base;
    }
    base.into_iter()
        .filter(|s| jgit_core::fuzzy_match(s, last))
        .collect()
}

/// Replaces the last token of `query` with `completion`.
pub fn complete_last_token(query: &str, completion: &str) -> String {
    let cut = query
        .rfind(|c: char| c.is_whitespace() || c == ',')
        .map_or(0, |i| i + 1);
    let (head, tail) = query.split_at(cut);
    let sign = if tail.starts_with('-') {
        "-"
    } else if tail.starts_with('+') {
        "+"
    } else {
        ""
    };
    format!("{head}{sign}{completion} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_buckets() {
        let now = 1_000_000;
        assert_eq!(format_relative_time(now - 30, now), "just now");
        assert_eq!(format_relative_time(now - 2 * 3_600, now), "2h ago");
        assert_eq!(format_relative_time(now - 90_000, now), "Yesterday");
        assert_eq!(format_relative_time(now - 3 * 86_400, now), "3d ago");
    }

    #[test]
    fn last_token_completion_keeps_sign() {
        assert_eq!(
            complete_last_token("main -dep", "dependabot/x"),
            "main -dependabot/x "
        );
        assert_eq!(complete_last_token("", "@me"), "@me ");
    }

    #[test]
    fn branches_pane_collapses_on_narrow_terminals() {
        let mut p = BranchesPaneState::default();
        assert!(p.is_collapsed(100));
        assert!(!p.is_collapsed(140));
        p.toggle_collapsed(100);
        assert!(!p.is_collapsed(100));
        p.toggle_collapsed(140);
        assert!(p.is_collapsed(140));
    }
}
