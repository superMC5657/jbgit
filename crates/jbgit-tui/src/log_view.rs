//! UI state for the `[2 Log]` three-pane workbench that is not graph
//! projection: Branches Dashboard, filter bar, presentation settings and the
//! right-hand changes / details / diff panes.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    BranchFilter, BranchTreeNode, BranchTreeRow, BranchesPanelSettings, BranchesTreeModel,
    ChangeRowKind, ChangeTreeRow, CommitChangesBundle, DateFilter, DetailsKey, DiffContext,
    EpochToken, FileChange, FileDiff, FileDiffKey, FileHistoryBundle, FileHistoryEntry,
    FileHistoryKey, LogFilterSpec, LruCache, Oid, PathFilter, RepoSnapshot, RequestEpoch,
    SignatureStatus, TextFilter, TextMatcher, UserFilter, build_change_rows, side_by_side_rows,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::syntax::{HighlightedDiff, LazyWordDiffCache};

/// Terminal width below which the Branches pane auto-collapses.
pub const NARROW_WIDTH: u16 = 110;
/// Debounce between a selection change and the details request.
pub const DETAILS_DEBOUNCE: Duration = Duration::from_millis(30);
pub const DETAILS_CACHE_CAPACITY: usize = 256;
pub const DIFF_CACHE_CAPACITY: usize = 64;
/// Quiet period after the last Log text-filter keystroke before the 50k-commit
/// graph is re-filtered (`Enter` applies immediately).
pub const FILTER_TEXT_DEBOUNCE: Duration = Duration::from_millis(120);
/// Per-snapshot memo of containing-branch BFS results.
pub const CONTAINING_CACHE_CAPACITY: usize = 256;

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
    /// Explicit search input mode in the Branches pane (`/` or `Ctrl+F`).
    pub searching: bool,
    /// `Alt+B` / `[«]` collapse on a wide terminal.
    pub hidden_by_user: bool,
    /// `Alt+B` on a narrow terminal re-opens the auto-collapsed pane.
    pub forced_open: bool,
    pub settings_path: Option<PathBuf>,
    source: Option<Arc<RepoSnapshot>>,
    ahead_behind_overrides: FxHashMap<String, (usize, usize)>,
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
        if stale {
            self.ahead_behind_overrides.clear();
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
        if !self.ahead_behind_overrides.is_empty() {
            for node in &mut self.model.nodes {
                if let Some(&ab) = self.ahead_behind_overrides.get(&node.key) {
                    node.ahead_behind = Some(ab);
                }
            }
        }
        self.source = snap.cloned();
        let valid: FxHashSet<&str> = self.model.nodes.iter().map(|n| n.key.as_str()).collect();
        self.multi.retain(|k| valid.contains(k.as_str()));
        self.refresh_rows();
        if let Some(key) = selected_key {
            self.select_key(&key);
        }
    }

    pub fn apply_ahead_behind_backfill(&mut self, updates: &[(String, (usize, usize))]) {
        if updates.is_empty() {
            return;
        }
        for (full_name, ab) in updates {
            self.ahead_behind_overrides.insert(full_name.clone(), *ab);
        }
        for node in &mut self.model.nodes {
            if let Some(&ab) = self.ahead_behind_overrides.get(&node.key) {
                node.ahead_behind = Some(ab);
            }
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

    pub fn apply_i18n(
        self,
        spec: &mut LogFilterSpec,
        expr: &str,
        now: i64,
        i18n: &crate::i18n::I18n,
    ) -> Result<(), String> {
        match self {
            Self::Date => {
                spec.date = DateFilter::parse(expr, now)
                    .ok_or_else(|| i18n.t_fmt("filter.date_parse_err", &[("expr", expr.trim())]))?;
                Ok(())
            }
            other => other.apply(spec, expr, now),
        }
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

#[derive(Debug, Clone)]
struct CachedTextMatcher {
    key: TextFilter,
    matcher: Option<Arc<TextMatcher>>,
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
    /// Debounced text-filter re-projection deadline (see [`FILTER_TEXT_DEBOUNCE`]).
    pub text_apply_at: Option<Instant>,
    compiled_text: RefCell<Option<CachedTextMatcher>>,
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

    /// Cached compiled text matcher rebuilt only when `spec.text` changes.
    pub fn compiled_text_matcher(&self) -> Option<Arc<TextMatcher>> {
        {
            let guard = self.compiled_text.borrow();
            if let Some(cached) = guard.as_ref()
                && cached.key == self.spec.text
            {
                return cached.matcher.clone();
            }
        }
        let compiled = self
            .spec
            .text
            .compile()
            .ok()
            .flatten()
            .map(Arc::new);
        *self.compiled_text.borrow_mut() = Some(CachedTextMatcher {
            key: self.spec.text.clone(),
            matcher: compiled.clone(),
        });
        compiled
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

    pub fn push_text_char(&mut self, ch: char, now: Instant) {
        self.spec.text.query.push(ch);
        self.text_apply_at = Some(now + FILTER_TEXT_DEBOUNCE);
    }

    pub fn pop_text_char(&mut self, now: Instant) -> bool {
        if self.spec.text.query.pop().is_some() {
            self.text_apply_at = Some(now + FILTER_TEXT_DEBOUNCE);
            true
        } else {
            false
        }
    }

    /// Leaves text-edit mode; returns `true` when an un-flushed edit was pending.
    pub fn finish_text_edit(&mut self) -> bool {
        self.editing_text = false;
        self.text_apply_at.take().is_some()
    }

    pub fn take_due_text_apply(&mut self, now: Instant) -> bool {
        if self.text_apply_at.is_some_and(|at| at <= now) {
            self.text_apply_at = None;
            true
        } else {
            false
        }
    }

    pub fn clear_text_debounce(&mut self) {
        self.text_apply_at = None;
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.text_apply_at
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesExpandState {
    AllExpanded,
    AllCollapsed,
    Mixed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    Header(usize),
    Unified(usize, usize),
    Split(usize, Option<usize>, Option<usize>),
}

pub fn build_file_diff_rows(diff: &FileDiff, side_by_side: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DiffRow::Header(h));
        if side_by_side {
            rows.extend(
                side_by_side_rows(hunk)
                    .into_iter()
                    .map(|r| DiffRow::Split(h, r.left, r.right)),
            );
        } else {
            rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Unified(h, l)));
        }
    }
    rows
}

#[derive(Debug)]
struct CachedFileDiffRows {
    diff: Arc<FileDiff>,
    side_by_side: bool,
    rows: Arc<[DiffRow]>,
}

#[derive(Debug, Default)]
pub(crate) struct FileDiffRowsCache {
    entry: RefCell<Option<CachedFileDiffRows>>,
    pub(crate) word_cache: LazyWordDiffCache<FileDiff>,
}

impl Clone for FileDiffRowsCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl FileDiffRowsCache {
    pub fn rows_for(&self, diff: &Arc<FileDiff>, side_by_side: bool) -> Arc<[DiffRow]> {
        {
            let guard = self.entry.borrow();
            if let Some(cached) = guard.as_ref()
                && cached.side_by_side == side_by_side
                && Arc::ptr_eq(&cached.diff, diff)
            {
                return Arc::clone(&cached.rows);
            }
        }
        let rows: Arc<[DiffRow]> = build_file_diff_rows(diff, side_by_side).into();
        *self.entry.borrow_mut() = Some(CachedFileDiffRows {
            diff: Arc::clone(diff),
            side_by_side,
            rows: Arc::clone(&rows),
        });
        rows
    }
}

#[derive(Debug)]
pub(crate) struct CachedChangeRows {
    bundle: Arc<CommitChangesBundle>,
    group_by_dir: bool,
    collapsed: FxHashSet<String>,
    rows: Arc<[ChangeTreeRow]>,
}

pub(crate) type CachedAllChangeRowsEntry = (Arc<CommitChangesBundle>, Arc<[ChangeTreeRow]>);

#[derive(Debug)]
pub struct DetailsPaneState {
    pub cache: LruCache<DetailsKey, Arc<CommitChangesBundle>>,
    pub diff_cache: LruCache<FileDiffKey, Arc<FileDiff>>,
    pub diff_syntax: LruCache<FileDiffKey, Arc<HighlightedDiff>>,
    pub signature_overrides: LruCache<Oid, SignatureStatus>,
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
    /// Background containing-branch results for the current snapshot.
    pub containing_cache: LruCache<Oid, Vec<String>>,
    /// Commit + epoch token of the in-flight containing-branch job.
    pub containing_requested: Option<(Oid, EpochToken)>,
    pub containing_epochs: RequestEpoch,

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
    /// Path to auto-select once the next `CommitChangesBundle` resolves.
    pub pending_select_path: Option<String>,

    pub(crate) cached_change_rows: RefCell<Option<CachedChangeRows>>,
    pub(crate) cached_all_change_rows: RefCell<Option<CachedAllChangeRowsEntry>>,
    pub(crate) diff_rows_cache: FileDiffRowsCache,
}

impl Default for DetailsPaneState {
    fn default() -> Self {
        Self {
            cache: LruCache::new(DETAILS_CACHE_CAPACITY),
            diff_cache: LruCache::new(DIFF_CACHE_CAPACITY),
            diff_syntax: LruCache::new(DIFF_CACHE_CAPACITY),
            signature_overrides: LruCache::new(DETAILS_CACHE_CAPACITY),
            current_key: None,
            compare_override: None,
            pending: None,
            merge_parent: 0,
            requested_diff: None,
            containing: None,
            containing_cache: LruCache::new(CONTAINING_CACHE_CAPACITY),
            containing_requested: None,
            containing_epochs: RequestEpoch::new(),
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
            pending_select_path: None,
            cached_change_rows: RefCell::new(None),
            cached_all_change_rows: RefCell::new(None),
            diff_rows_cache: FileDiffRowsCache::default(),
        }
    }
}

impl DetailsPaneState {
    pub fn current_bundle(&self) -> Option<&Arc<CommitChangesBundle>> {
        self.cache.peek(self.current_key.as_ref()?)
    }

    pub fn change_rows(&self) -> Arc<[ChangeTreeRow]> {
        let Some(bundle) = self.current_bundle() else {
            return Arc::from([]);
        };
        {
            let guard = self.cached_change_rows.borrow();
            if let Some(cached) = guard.as_ref()
                && Arc::ptr_eq(&cached.bundle, bundle)
                && cached.group_by_dir == self.changes_group_by_dir
                && cached.collapsed == self.changes_collapsed
            {
                return Arc::clone(&cached.rows);
            }
        }
        let rows: Arc<[ChangeTreeRow]> = build_change_rows(
            &bundle.changes,
            self.changes_group_by_dir,
            &self.changes_collapsed,
        )
        .into();
        *self.cached_change_rows.borrow_mut() = Some(CachedChangeRows {
            bundle: Arc::clone(bundle),
            group_by_dir: self.changes_group_by_dir,
            collapsed: self.changes_collapsed.clone(),
            rows: Arc::clone(&rows),
        });
        rows
    }

    pub fn all_expanded_change_rows(&self) -> Option<Arc<[ChangeTreeRow]>> {
        let bundle = self.current_bundle()?;
        {
            let guard = self.cached_all_change_rows.borrow();
            if let Some((b, rows)) = guard.as_ref()
                && Arc::ptr_eq(b, bundle)
            {
                return Some(Arc::clone(rows));
            }
        }
        let rows: Arc<[ChangeTreeRow]> =
            build_change_rows(&bundle.changes, true, &FxHashSet::default()).into();
        *self.cached_all_change_rows.borrow_mut() =
            Some((Arc::clone(bundle), Arc::clone(&rows)));
        Some(rows)
    }

    pub fn diff_rows_for(&self, diff: &Arc<FileDiff>, side_by_side: bool) -> Arc<[DiffRow]> {
        self.diff_rows_cache.rows_for(diff, side_by_side)
    }

    /// Returns the expansion state for the current changes tree's root directories.
    pub fn changes_expand_state(&self) -> ChangesExpandState {
        if !self.changes_group_by_dir {
            return ChangesExpandState::Unavailable;
        }
        let Some(all) = self.all_expanded_change_rows() else {
            return ChangesExpandState::Unavailable;
        };
        let mut has_dirs = false;
        let mut all_expanded = true;
        let mut all_collapsed = true;
        for row in all.iter() {
            if matches!(row.kind, ChangeRowKind::Dir { .. }) {
                has_dirs = true;
                let collapsed = self.changes_collapsed.contains(&row.key);
                if collapsed {
                    all_expanded = false;
                } else if row.depth == 0 {
                    all_collapsed = false;
                }
            }
        }
        match (has_dirs, all_expanded, all_collapsed) {
            (false, _, _) => ChangesExpandState::Unavailable,
            (true, true, false) => ChangesExpandState::AllExpanded,
            (true, false, true) => ChangesExpandState::AllCollapsed,
            _ => ChangesExpandState::Mixed,
        }
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

    /// Fallback upper scroll bound used before the diff pane has rendered a frame.
    pub fn fallback_diff_max_scroll(&self) -> usize {
        self.current_diff()
            .map_or(usize::MAX, |_| self.diff_total_rows().saturating_sub(1))
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
    }

    pub fn reset_containing(&mut self) {
        self.containing = None;
        self.containing_cache.clear();
        self.containing_requested = None;
        let _ = self.containing_epochs.issue();
    }

    /// Resolves a single-commit containing-branch query from cache or issues a
    /// new cancellable [`EpochToken`] when an off-thread BFS is needed.
    pub fn request_containing_branches(&mut self, oid: Oid) -> Option<EpochToken> {
        if self.pending.is_some()
            || self.containing.as_ref().is_some_and(|(o, _)| *o == oid)
        {
            return None;
        }
        if let Some(names) = self.containing_cache.get(&oid).cloned() {
            self.containing = Some((oid, names));
            return None;
        }
        if self
            .containing_requested
            .as_ref()
            .is_some_and(|(o, _)| *o == oid)
        {
            return None;
        }
        let token = self.containing_epochs.issue();
        self.containing_requested = Some((oid, token.clone()));
        Some(token)
    }

    /// Applies an off-thread containing-branch result if it is still current.
    /// Returns `true` when the active pane display changed.
    pub fn resolve_containing_branches(
        &mut self,
        oid: Oid,
        token: &EpochToken,
        names: Vec<String>,
    ) -> bool {
        let is_current = self
            .containing_requested
            .as_ref()
            .is_some_and(|(o, t)| *o == oid && t.epoch() == token.epoch() && !token.is_stale());
        if !is_current {
            return false;
        }
        self.containing_requested = None;
        self.containing_cache.insert(oid, names.clone());
        let selected = self
            .current_key
            .as_ref()
            .is_some_and(|k| k.oids.as_slice() == [oid]);
        if selected {
            self.containing = Some((oid, names));
            true
        } else {
            false
        }
    }

    pub fn diff_total_rows(&self) -> usize {
        let Some(diff) = self.current_diff() else {
            return 0;
        };
        if diff.is_too_large || diff.is_binary || diff.hunks.is_empty() {
            return 0;
        }
        self.diff_rows_for(diff, self.diff_side_by_side).len() + usize::from(diff.is_truncated)
    }

    pub fn hunk_header_offsets(&self) -> Vec<usize> {
        let Some(diff) = self.current_diff() else {
            return Vec::new();
        };
        self.diff_rows_for(diff, self.diff_side_by_side)
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| matches!(r, DiffRow::Header(_)).then_some(idx))
            .collect()
    }

    pub fn jump_diff_hunk(&mut self, delta: isize) {
        let offsets = self.hunk_header_offsets();
        if offsets.is_empty() {
            return;
        }
        let cur = self.diff_scroll;
        let target = if delta >= 0 {
            offsets.iter().copied().find(|&o| o > cur).unwrap_or(offsets[0])
        } else {
            offsets
                .iter()
                .copied()
                .rev()
                .find(|&o| o < cur)
                .unwrap_or(*offsets.last().unwrap())
        };
        self.diff_scroll = target;
    }

    pub fn select_next_file(&mut self, delta: isize) {
        let files = self.file_row_positions();
        if files.is_empty() {
            return;
        }
        let cur = files
            .iter()
            .position(|&p| p == self.changes_selected)
            .map_or(0, |i| i as isize + delta);
        let next = cur.clamp(0, files.len() as isize - 1) as usize;
        self.changes_selected = files[next];
        self.diff_scroll = 0;
    }

    /// Resolves `pending_select_path` once the current details bundle is available,
    /// expanding any collapsed ancestor directories so the file row is visible.
    pub fn try_select_pending_path(&mut self) -> Option<(String, bool)> {
        let bundle = self.current_bundle()?.clone();
        let path = self.pending_select_path.take()?;
        if bundle.changes.iter().any(|c| c.path == path) {
            let to_expand: Vec<String> = self
                .changes_collapsed
                .iter()
                .filter(|dir| {
                    path.starts_with(dir.as_str())
                        && path.as_bytes().get(dir.len()) == Some(&b'/')
                })
                .cloned()
                .collect();
            for dir in to_expand {
                self.changes_collapsed.remove(&dir);
            }
            if let Some(idx) = self.change_rows().iter().position(|r| {
                matches!(r.kind, ChangeRowKind::File { .. }) && r.key == path
            }) {
                self.changes_selected = idx;
                self.diff_scroll = 0;
                return Some((path, true));
            }
        }
        Some((path, false))
    }
}

// ───────────────────────────── Single-file history ─────────────────────────────

/// Chord window in `PopupKind::FileHistory` to disambiguate `gg` (jump to top) from `g` (locate in log).
pub const FILE_HISTORY_G_WINDOW: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileHistoryFocus {
    #[default]
    Commits,
    Diff,
}

#[derive(Debug)]
pub struct FileHistoryState {
    pub key: FileHistoryKey,
    pub bundle: Option<Arc<FileHistoryBundle>>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    /// Anchor row for two-revision comparison (`Shift+Up`/`Down` or `Shift+Click`).
    pub anchor: Option<usize>,
    pub compare_with_local: bool,
    pub diff_side_by_side: bool,
    pub diff_context: DiffContext,
    pub diff_scroll: usize,
    pub diff_zoomed: bool,
    pub focus: FileHistoryFocus,
    pub pending_diff: Option<(FileDiffKey, Instant)>,
    pub requested_diff: Option<FileDiffKey>,
    pub pending_g: Option<Instant>,
    pub(crate) diff_rows_cache: FileDiffRowsCache,
}

impl FileHistoryState {
    pub fn new(key: FileHistoryKey, diff_context: DiffContext) -> Self {
        Self {
            key,
            bundle: None,
            loading: true,
            error: None,
            selected: 0,
            anchor: None,
            compare_with_local: false,
            diff_side_by_side: false,
            diff_context,
            diff_scroll: 0,
            diff_zoomed: false,
            focus: FileHistoryFocus::Commits,
            pending_diff: None,
            requested_diff: None,
            pending_g: None,
            diff_rows_cache: FileDiffRowsCache::default(),
        }
    }

    pub fn diff_rows_for(&self, diff: &Arc<FileDiff>) -> Arc<[DiffRow]> {
        self.diff_rows_cache.rows_for(diff, self.diff_side_by_side)
    }

    pub fn entries(&self) -> &[FileHistoryEntry] {
        self.bundle.as_deref().map_or(&[], |b| b.entries.as_slice())
    }

    pub fn selected_entry(&self) -> Option<&FileHistoryEntry> {
        self.entries().get(self.selected)
    }

    /// When two distinct commits are selected via `anchor`, returns `(older, newer)`.
    pub fn comparison_pair(&self) -> Option<(&FileHistoryEntry, &FileHistoryEntry)> {
        let entries = self.entries();
        let a = self.anchor?;
        if a == self.selected || a >= entries.len() || self.selected >= entries.len() {
            return None;
        }
        let newer_idx = self.selected.min(a);
        let older_idx = self.selected.max(a);
        Some((&entries[older_idx], &entries[newer_idx]))
    }

    pub fn desired_diff_key(&self) -> Option<FileDiffKey> {
        let cur = self.selected_entry()?;
        if self.compare_with_local {
            Some(FileDiffKey {
                details: DetailsKey::compare_with_local(cur.oid),
                path: self.key.path.clone(),
                old_path: (cur.path_at_commit != self.key.path)
                    .then(|| cur.path_at_commit.clone()),
                context: self.diff_context,
            })
        } else if let Some((older, newer)) = self.comparison_pair() {
            Some(FileDiffKey {
                details: DetailsKey::compare_versions(older.oid, newer.oid),
                path: newer.path_at_commit.clone(),
                old_path: (older.path_at_commit != newer.path_at_commit)
                    .then(|| older.path_at_commit.clone()),
                context: self.diff_context,
            })
        } else {
            Some(FileDiffKey {
                details: DetailsKey::single(cur.oid),
                path: cur.path_at_commit.clone(),
                old_path: cur.old_path.clone(),
                context: self.diff_context,
            })
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pending_diff.as_ref().map(|(_, at)| *at),
            self.pending_g.map(|t| t + FILE_HISTORY_G_WINDOW),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn apply_selection(&mut self, next: usize, extend: bool) {
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.selected);
        } else if !extend {
            self.anchor = None;
        }
        if next != self.selected || !extend {
            self.selected = next;
            self.diff_scroll = 0;
        }
    }

    pub fn move_selection(&mut self, delta: isize, extend: bool) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        self.apply_selection(next, extend);
    }

    pub fn select_row(&mut self, idx: usize, extend: bool) {
        let len = self.entries().len();
        if len == 0 {
            return;
        }
        self.apply_selection(idx.min(len - 1), extend);
    }

    pub fn toggle_follow(&mut self) -> FileHistoryKey {
        self.key.follow_renames = !self.key.follow_renames;
        self.loading = true;
        self.error = None;
        self.anchor = None;
        self.diff_scroll = 0;
        self.pending_diff = None;
        self.requested_diff = None;
        self.key.clone()
    }

    pub fn toggle_compare_local(&mut self) {
        self.compare_with_local = !self.compare_with_local;
        self.diff_scroll = 0;
        self.pending_diff = None;
    }

    pub fn toggle_side_by_side(&mut self) {
        self.diff_side_by_side = !self.diff_side_by_side;
        self.diff_scroll = 0;
    }

    pub fn cycle_context(&mut self) {
        self.diff_context = self.diff_context.next();
        self.diff_scroll = 0;
        self.pending_diff = None;
    }

    pub fn toggle_zoom(&mut self) {
        self.diff_zoomed = !self.diff_zoomed;
    }

    pub fn set_focus(&mut self, focus: FileHistoryFocus) {
        self.focus = focus;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FileHistoryFocus::Commits => FileHistoryFocus::Diff,
            FileHistoryFocus::Diff => FileHistoryFocus::Commits,
        };
    }

    pub fn fallback_diff_max_scroll(&self, diff_cache: &LruCache<FileDiffKey, Arc<FileDiff>>) -> usize {
        self.desired_diff_key()
            .and_then(|k| diff_cache.peek(&k))
            .map_or(usize::MAX, |d| {
                if d.is_too_large || d.is_binary || d.hunks.is_empty() {
                    0
                } else {
                    (self.diff_rows_for(d).len() + usize::from(d.is_truncated)).saturating_sub(1)
                }
            })
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
        .filter(|s| jbgit_core::fuzzy_match(s, last))
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

    #[test]
    fn details_pane_change_rows_caches_and_invalidates_on_collapsed_or_bundle() {
        let mut d = DetailsPaneState::default();
        let oid = jbgit_core::Oid::from_bytes([1u8; 20]);
        let key = DetailsKey::single(oid);
        let bundle1 = Arc::new(jbgit_core::CommitChangesBundle {
            key: key.clone(),
            details: vec![],
            changes: vec![jbgit_core::FileChange {
                path: "src/a.rs".into(),
                old_path: None,
                kind: jbgit_core::FileStatusKind::Modified,
                additions: 1,
                deletions: 0,
                is_binary: false,
                exists_in_workdir: true,
                old_oid: None,
                new_oid: None,
            }],
        });
        d.cache.insert(key.clone(), Arc::clone(&bundle1));
        d.current_key = Some(key.clone());

        // 1. Initial call caches Arc<[ChangeTreeRow]>
        let r1 = d.change_rows();
        let r2 = d.change_rows();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "change_rows must return the same Arc slice on identical bundle"
        );

        // 2. Modifying collapsed set invalidates cache
        d.changes_collapsed.insert("src".into());
        let r3 = d.change_rows();
        assert!(
            !Arc::ptr_eq(&r1, &r3),
            "change_rows must invalidate cache when collapsed set changes"
        );

        // 3. Diff rows cache tests
        let diff = Arc::new(jbgit_core::FileDiff {
            key: jbgit_core::FileDiffKey {
                details: DetailsKey::single(oid),
                path: "src/a.rs".into(),
                old_path: None,
                context: DiffContext::Three,
            },
            hunks: vec![],
            is_binary: false,
            is_truncated: false,
            is_too_large: false,
        });
        let dr1 = d.diff_rows_for(&diff, false);
        let dr2 = d.diff_rows_for(&diff, false);
        assert!(
            Arc::ptr_eq(&dr1, &dr2),
            "diff_rows_for must cache rows for same diff and mode"
        );
        let dr3 = d.diff_rows_for(&diff, true);
        assert!(
            !Arc::ptr_eq(&dr1, &dr3),
            "diff_rows_for must invalidate when side_by_side changes"
        );
    }
}
