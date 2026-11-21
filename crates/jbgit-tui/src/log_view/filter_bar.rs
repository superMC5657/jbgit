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
