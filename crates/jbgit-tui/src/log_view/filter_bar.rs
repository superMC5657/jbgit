use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jbgit_core::{
    BranchFilter, DateFilter, LogFilterSpec, Oid, PathFilter, RepoSnapshot, TextFilter,
    TextMatcher, UserFilter,
};

/// Quiet period after the last Log text-filter keystroke before the 50k-commit
/// graph is re-filtered (`Enter` applies immediately).
pub const FILTER_TEXT_DEBOUNCE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterField {
    Branch,
    User,
    Date,
    Paths,
}

impl FilterField {
    pub const ALL: [Self; 4] = [Self::Branch, Self::User, Self::Date, Self::Paths];

    pub const fn index(self) -> usize {
        match self {
            Self::Branch => 0,
            Self::User => 1,
            Self::Date => 2,
            Self::Paths => 3,
        }
    }

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
            Self::Branch => "names, a..b, Favorites, -exclude   e.g. main..feat Favorites",
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

/// Maximum number of recent filter queries retained per filter dimension.
pub const MAX_FILTER_HISTORY: usize = 10;

/// In-table speed search state for the Log commits table (`VcsLogSpeedSearch`).
///
/// Unlike [`FilterBarState`], speed search NEVER hides non-matching commits: it
/// highlights matching rows in place and steps between them with `n` / `N`.
#[derive(Debug, Default, Clone)]
pub struct LogSpeedSearchState {
    /// Speed search bar is visible / active.
    pub active: bool,
    /// Typing into the speed search input (`Enter` confirms to browse mode; `Esc` clears).
    pub editing: bool,
    pub query: String,
    /// Visible row indices matching `query` (in ascending order).
    pub matches: Vec<usize>,
    /// Index into `matches` of the currently focused match.
    pub current: Option<usize>,
    /// `LogGraphState::visible_revision` at which `matches` was last computed.
    pub graph_revision: u64,
}

impl LogSpeedSearchState {
    pub fn open(&mut self) {
        self.active = true;
        self.editing = true;
    }

    pub fn close(&mut self) {
        self.active = false;
        self.editing = false;
        self.query.clear();
        self.matches.clear();
        self.current = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn query_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let q = self.query.trim();
        if !self.active || q.is_empty() {
            return Vec::new();
        }
        let q_low = q.to_lowercase();
        if q_low.is_empty() {
            return Vec::new();
        }
        if text.is_ascii() && q_low.is_ascii() {
            let mut ranges = Vec::new();
            let hay = text.as_bytes();
            let needle = q_low.as_bytes();
            let mut i = 0;
            while i + needle.len() <= hay.len() {
                if hay[i..i + needle.len()].eq_ignore_ascii_case(needle) {
                    ranges.push((i, i + needle.len()));
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
            return ranges;
        }
        // Unicode fallback: match on lowercased string when char lengths match.
        let low = text.to_lowercase();
        if low.len() == text.len() {
            let mut ranges = Vec::new();
            let mut start = 0;
            while let Some(pos) = low[start..].find(&q_low) {
                let s = start + pos;
                let e = s + q_low.len();
                if text.is_char_boundary(s) && text.is_char_boundary(e) {
                    ranges.push((s, e));
                }
                start = e;
            }
            return ranges;
        }
        Vec::new()
    }

    pub fn is_row_match(&self, row: usize) -> bool {
        self.active && self.matches.binary_search(&row).is_ok()
    }

    /// Recomputes `matches` against the currently visible rows and returns the
    /// row to focus (nearest match at or after `cursor_row`, wrapping around).
    pub fn recompute(
        &mut self,
        cached: &crate::graph::state::CachedLogGraph,
        snap: &RepoSnapshot,
        cursor_row: usize,
    ) -> Option<usize> {
        self.matches.clear();
        self.current = None;
        let q = self.query.trim();
        if !self.active || q.is_empty() {
            return None;
        }
        let q_low = q.to_lowercase();
        let total = cached.visible_count();
        for row in 0..total {
            let Some(commit) = cached.commit_at_visible_row(snap, row) else {
                continue;
            };
            let hit = commit.subject.to_lowercase().contains(&q_low)
                || commit.author_name.to_lowercase().contains(&q_low)
                || commit.oid.matches_hex_prefix(&q_low)
                || cached
                    .refs_for_oid(snap, commit.oid)
                    .any(|r| r.name.to_lowercase().contains(&q_low));
            if hit {
                self.matches.push(row);
            }
        }
        if self.matches.is_empty() {
            return None;
        }
        let idx = self
            .matches
            .partition_point(|&r| r < cursor_row)
            .min(self.matches.len() - 1);
        let idx = if self.matches.get(idx).is_some_and(|&r| r >= cursor_row) {
            idx
        } else {
            0
        };
        self.current = Some(idx);
        Some(self.matches[idx])
    }

    /// Steps to the next (`forward == true`) or previous match, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<usize> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let len = self.matches.len();
        let next = match self.current {
            Some(i) if forward => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
            None if forward => 0,
            None => len - 1,
        };
        self.current = Some(next);
        Some(self.matches[next])
    }
}

#[derive(Debug, Default)]
pub struct FilterBarState {
    pub spec: LogFilterSpec,
    /// Snapshot of `spec` saved when entering Directory History so exiting
    /// restores the user's previous Log filter losslessly.
    pub saved_before_dir_history: Option<LogFilterSpec>,
    /// Text box has keyboard focus (`Ctrl+L` / `/`).
    pub editing_text: bool,
    /// Path matches for `spec.paths` (paths they were computed for + set).
    pub path_matches: Option<PathMatches>,
    /// Paths + snapshot of the in-flight path request.
    pub path_requested: Option<PathRequest>,
    /// Debounced text-filter re-projection deadline (see [`FILTER_TEXT_DEBOUNCE`]).
    pub text_apply_at: Option<Instant>,
    /// Last 10 applied queries per [`FilterField`], newest first.
    pub history: [Vec<String>; 4],
    compiled_text: RefCell<Option<CachedTextMatcher>>,
}

impl FilterBarState {
    pub fn record_history(&mut self, field: FilterField, expr: &str) {
        let trimmed = expr.trim();
        if trimmed.is_empty() {
            return;
        }
        let list = &mut self.history[field.index()];
        list.retain(|e| e != trimmed);
        list.insert(0, trimmed.to_string());
        list.truncate(MAX_FILTER_HISTORY);
    }

    pub fn history_for(&self, field: FilterField) -> &[String] {
        &self.history[field.index()]
    }
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
        let compiled = self.spec.text.compile().ok().flatten().map(Arc::new);
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

/// Suggestions for a filter field editor, incorporating built-in `Favorites`
/// (when favorite branches exist) and the per-field MRU history (up to 10 items).
pub fn filter_suggestions_with_context(
    field: FilterField,
    snap: Option<&RepoSnapshot>,
    query: &str,
    has_favorites: bool,
    history: &[String],
) -> Vec<String> {
    let last = query
        .rsplit(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("")
        .trim_start_matches(['-', '+']);
    let mut base: Vec<String> = Vec::new();
    let mut push_unique = |item: String| {
        if !base.contains(&item) {
            base.push(item);
        }
    };
    match field {
        FilterField::Branch => {
            push_unique("HEAD".to_string());
            if has_favorites {
                push_unique("Favorites".to_string());
            }
            for h in history {
                push_unique(h.clone());
            }
            if let Some(s) = snap {
                for r in &s.refs {
                    push_unique(r.name.clone());
                }
            }
        }
        FilterField::User => {
            push_unique("@me".to_string());
            for h in history {
                push_unique(h.clone());
            }
            if let Some(s) = snap {
                let mut counts: rustc_hash::FxHashMap<&str, usize> = Default::default();
                for c in &s.commits {
                    *counts.entry(c.author_name.as_str()).or_default() += 1;
                }
                let mut authors: Vec<_> = counts.into_iter().collect();
                authors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                for (a, _) in authors.into_iter().take(200) {
                    push_unique(a.to_string());
                }
            }
        }
        FilterField::Date => {
            for h in history {
                push_unique(h.clone());
            }
            for preset in ["today", "yesterday", "7d", "30d", "all"] {
                push_unique(preset.to_string());
            }
        }
        FilterField::Paths => {
            for h in history {
                push_unique(h.clone());
            }
            if let Some(s) = snap
                && let Ok(entries) = std::fs::read_dir(&s.workdir)
            {
                let mut dirs = Vec::new();
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !name.starts_with('.') {
                        dirs.push(name);
                    }
                }
                dirs.sort();
                for d in dirs {
                    push_unique(d);
                }
            }
        }
    }
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
