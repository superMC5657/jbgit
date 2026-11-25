//! Multi-dimensional log filter (`VcsLogFilterCollection` in IDEA):
//! text / hash, branch, user, date and path constraints intersected into a
//! single matching-node bitset for [`crate::graph::VisibleGraph::build`].

use std::collections::HashSet;

use chrono::{Local, NaiveDate, TimeZone};
use fixedbitset::FixedBitSet;
use regex::{Regex, RegexBuilder};
use thiserror::Error;

use crate::graph::{PermanentLinearGraph, ReachableNodes};
use crate::{Oid, RefInfo, RepoSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FilterError {
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
}

/// Result of evaluating a [`LogFilterSpec`]: `(matching_nodes, branch_start_heads)`.
pub type FilterMatchWithHeads = (Option<FixedBitSet>, Option<Vec<crate::graph::PermNodeId>>);

/// Text / hash search (`[.*]` regex and `[Cc]` match-case toggles).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextFilter {
    pub query: String,
    pub regex: bool,
    pub match_case: bool,
}

/// Compiled text matcher; plain text is compiled as an escaped regex so
/// case-insensitive highlighting stays byte-accurate for Unicode.
#[derive(Debug, Clone)]
pub struct TextMatcher {
    re: Regex,
    /// One or more space-separated 4..=40 hex commit hash prefixes.
    hash_prefixes: Vec<String>,
}

fn is_hex_hash_token(tok: &str) -> bool {
    (4..=40).contains(&tok.len()) && tok.chars().all(|c| c.is_ascii_hexdigit())
}

impl TextFilter {
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty()
    }

    pub fn compile(&self) -> Result<Option<TextMatcher>, FilterError> {
        let q = self.query.trim();
        if q.is_empty() {
            return Ok(None);
        }
        let pattern = if self.regex {
            q.to_string()
        } else {
            regex::escape(q)
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!self.match_case)
            .build()
            .map_err(|e| FilterError::InvalidRegex(e.to_string()))?;
        let hash_prefixes = if self.regex {
            if is_hex_hash_token(q) {
                vec![q.to_ascii_lowercase()]
            } else {
                Vec::new()
            }
        } else {
            let tokens: Vec<&str> = q.split_whitespace().collect();
            if !tokens.is_empty() && tokens.iter().all(|t| is_hex_hash_token(t)) {
                tokens.into_iter().map(str::to_ascii_lowercase).collect()
            } else {
                Vec::new()
            }
        };
        Ok(Some(TextMatcher { re, hash_prefixes }))
    }
}

impl TextMatcher {
    pub fn matches_commit(&self, oid: &Oid, subject: &str, body: &str) -> bool {
        self.hash_prefixes.iter().any(|p| oid.matches_hex_prefix(p))
            || self.re.is_match(subject)
            || (!body.is_empty() && self.re.is_match(body))
    }

    /// Byte ranges of every match in `text` (for search-hit highlighting).
    pub fn find_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        self.re
            .find_iter(text)
            .filter(|m| m.start() < m.end())
            .map(|m| (m.start(), m.end()))
            .collect()
    }
}

/// Branch include / exclude / range rules (`main feat/* -dependabot/* main..feat Favorites`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Revision ranges `a..b` (commits reachable from `b` excluding `a`).
    pub ranges: Vec<(String, String)>,
}

impl BranchFilter {
    pub fn parse(expr: &str) -> Self {
        let mut out = Self::default();
        for tok in expr
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
        {
            if let Some(ex) = tok.strip_prefix('-') {
                if !ex.is_empty() && !out.exclude.iter().any(|e| e == ex) {
                    out.exclude.push(ex.to_string());
                }
            } else if let Some((from, to)) = tok.split_once("..")
                && !from.is_empty()
                && !to.is_empty()
                && !to.starts_with('.')
                && !to.contains("..")
            {
                let pair = (from.to_string(), to.to_string());
                if !out.ranges.contains(&pair) {
                    out.ranges.push(pair);
                }
            } else {
                let inc = tok.strip_prefix('+').unwrap_or(tok);
                if !inc.is_empty() && !out.include.iter().any(|e| e == inc) {
                    out.include.push(inc.to_string());
                }
            }
        }
        out
    }

    pub fn from_names(names: Vec<String>) -> Self {
        Self {
            include: names,
            exclude: Vec::new(),
            ranges: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty() && self.ranges.is_empty()
    }

    pub fn to_expr(&self) -> String {
        self.include
            .iter()
            .cloned()
            .chain(self.ranges.iter().map(|(a, b)| format!("{a}..{b}")))
            .chain(self.exclude.iter().map(|e| format!("-{e}")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Compact chip label: `main + feat/*`, `main..feat`, `main +2`, `-dependabot/*`.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        let positives: Vec<String> = self
            .include
            .iter()
            .cloned()
            .chain(self.ranges.iter().map(|(a, b)| format!("{a}..{b}")))
            .collect();
        match positives.len() {
            0 => {}
            1 | 2 => parts.push(positives.join(" + ")),
            n => parts.push(format!("{} +{}", positives[0], n - 1)),
        }
        parts.extend(self.exclude.iter().map(|e| format!("-{e}")));
        if parts.is_empty() {
            "All".to_string()
        } else {
            parts.join(" ")
        }
    }

    fn is_favorites_token(tok: &str) -> bool {
        tok.eq_ignore_ascii_case("favorites") || tok.eq_ignore_ascii_case("@favorites")
    }

    fn matches_current_branch(tok: &str, head_branch: Option<&str>) -> bool {
        tok.eq_ignore_ascii_case("HEAD")
            || head_branch.is_some_and(|b| tok == b || tok == format!("refs/heads/{b}"))
    }

    /// True when this branch filter restricts visibility strictly to `HEAD` or
    /// the current branch itself (aligned with `CurrentBranchHighlighter.java:49-56`).
    pub fn is_only_current_branch(&self, head_branch: Option<&str>) -> bool {
        self.exclude.is_empty()
            && self.ranges.is_empty()
            && self.include.len() == 1
            && Self::matches_current_branch(&self.include[0], head_branch)
    }

    /// Returns a single non-current branch being inspected or compared against
    /// `HEAD` (used to drive the async `Already Cherry-Picked` highlighter).
    pub fn single_compared_branch<'a>(&'a self, head_branch: Option<&str>) -> Option<&'a str> {
        let is_concrete_other = |tok: &str| -> bool {
            !tok.is_empty()
                && !tok.contains('*')
                && !tok.contains('?')
                && !Self::is_favorites_token(tok)
                && !Self::matches_current_branch(tok, head_branch)
        };
        if !self.exclude.is_empty() {
            return None;
        }
        if self.ranges.is_empty() {
            match self.include.as_slice() {
                [only] if is_concrete_other(only) => Some(only.as_str()),
                [a, b] if Self::matches_current_branch(a, head_branch) && is_concrete_other(b) => {
                    Some(b.as_str())
                }
                [a, b] if Self::matches_current_branch(b, head_branch) && is_concrete_other(a) => {
                    Some(a.as_str())
                }
                _ => None,
            }
        } else if self.include.is_empty() && self.ranges.len() == 1 {
            let (from, to) = &self.ranges[0];
            if Self::matches_current_branch(from, head_branch) && is_concrete_other(to) {
                Some(to.as_str())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Expands any `Favorites` / `@favorites` placeholder tokens using `favorites`.
    pub fn expand_favorites(&self, favorites: &[String]) -> Self {
        if !self.include.iter().any(|t| Self::is_favorites_token(t)) {
            return self.clone();
        }
        let mut include = Vec::new();
        for tok in &self.include {
            if Self::is_favorites_token(tok) {
                for fav in favorites {
                    if !include.contains(fav) {
                        include.push(fav.clone());
                    }
                }
            } else if !include.contains(tok) {
                include.push(tok.clone());
            }
        }
        Self {
            include,
            exclude: self.exclude.clone(),
            ranges: self.ranges.clone(),
        }
    }

    fn ref_matches(pattern: &str, r: &RefInfo) -> bool {
        glob_match(pattern, &r.name) || glob_match(pattern, &r.full_name)
    }

    /// Head commits whose ancestry forms the visible set (for `include`/`exclude` rules).
    pub fn select_heads(
        &self,
        refs: &[RefInfo],
        head_branch: Option<&str>,
        head_oid: Option<Oid>,
    ) -> Vec<Oid> {
        let mut out = Vec::new();
        let include_all = self.include.is_empty() && self.ranges.is_empty();
        let head_matches = |pattern: &str| -> bool {
            glob_match(pattern, "HEAD")
                || head_branch.is_some_and(|b| {
                    glob_match(pattern, b) || glob_match(pattern, &format!("refs/heads/{b}"))
                })
        };
        if let Some(head) = head_oid
            && (include_all || self.include.iter().any(|p| head_matches(p)))
            && !self.exclude.iter().any(|p| head_matches(p))
        {
            out.push(head);
        }
        for r in refs {
            let included = include_all || self.include.iter().any(|p| Self::ref_matches(p, r));
            if included && !self.exclude.iter().any(|p| Self::ref_matches(p, r)) {
                out.push(r.target_oid);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Resolves `include`, `exclude`, `ranges` (`a..b`), and `Favorites` into a
    /// visible node bitset plus the resolved head `PermNodeId`s for first-parent walks.
    pub fn compute_mask_and_heads(
        &self,
        graph: &PermanentLinearGraph,
        snapshot: &RepoSnapshot,
        favorites: &[String],
    ) -> (FixedBitSet, Vec<crate::graph::PermNodeId>) {
        let n = graph.nodes_count as usize;
        let expanded = self.expand_favorites(favorites);
        // If user explicitly asked for `Favorites` (even with excludes) and has no favorites, match nothing.
        let asked_only_empty_favorites = self.include.iter().all(|t| Self::is_favorites_token(t))
            && !self.include.is_empty()
            && expanded.include.is_empty()
            && self.ranges.is_empty();
        if asked_only_empty_favorites {
            return (FixedBitSet::with_capacity(n), Vec::new());
        }

        let mut start_heads: Vec<crate::graph::PermNodeId> = Vec::new();
        let mut mask = FixedBitSet::with_capacity(n);

        let has_branch_rules = !expanded.include.is_empty()
            || !expanded.exclude.is_empty()
            || expanded.ranges.is_empty();
        if has_branch_rules {
            let heads: Vec<_> = expanded
                .select_heads(
                    &snapshot.refs,
                    snapshot.head_branch.as_deref(),
                    snapshot.head_oid,
                )
                .iter()
                .filter_map(|oid| graph.node_of_oid(oid))
                .collect();
            let branch_reach = ReachableNodes::contained_in_branch(graph, &heads);
            mask.union_with(&branch_reach);
            start_heads.extend(heads);
        }

        let exclude_reach = if !expanded.exclude.is_empty() && !expanded.ranges.is_empty() {
            let head_matches = |pattern: &str| -> bool {
                glob_match(pattern, "HEAD")
                    || snapshot.head_branch.as_deref().is_some_and(|b| {
                        glob_match(pattern, b) || glob_match(pattern, &format!("refs/heads/{b}"))
                    })
            };
            let mut excl_heads: Vec<_> = snapshot
                .refs
                .iter()
                .filter(|r| expanded.exclude.iter().any(|p| Self::ref_matches(p, r)))
                .filter_map(|r| graph.node_of_oid(&r.target_oid))
                .collect();
            if let Some(head) = snapshot.head_oid
                && expanded.exclude.iter().any(|p| head_matches(p))
                && let Some(node) = graph.node_of_oid(&head)
            {
                excl_heads.push(node);
            }
            if excl_heads.is_empty() {
                None
            } else {
                Some(ReachableNodes::contained_in_branch(graph, &excl_heads))
            }
        } else {
            None
        };

        for (from, to) in &expanded.ranges {
            let to_node = snapshot
                .resolve_revision(to)
                .and_then(|oid| graph.node_of_oid(&oid));
            let from_node = snapshot
                .resolve_revision(from)
                .and_then(|oid| graph.node_of_oid(&oid));
            if let (Some(f), Some(t)) = (from_node, to_node) {
                let mut reach_to = ReachableNodes::contained_in_branch(graph, &[t]);
                let reach_from = ReachableNodes::contained_in_branch(graph, &[f]);
                reach_to.difference_with(&reach_from);
                if let Some(ref excl) = exclude_reach {
                    reach_to.difference_with(excl);
                }
                mask.union_with(&reach_to);
                start_heads.push(t);
            }
        }

        start_heads.sort_unstable();
        start_heads.dedup();
        (mask, start_heads)
    }
}

/// Configured Git user identity (`user.name` / `user.email`) for `@me`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepoUser<'a> {
    pub name: Option<&'a str>,
    pub email: Option<&'a str>,
}

/// Author filter: `@me` plus case-insensitive name / email fragments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserFilter {
    pub users: Vec<String>,
}

impl UserFilter {
    pub fn parse(expr: &str) -> Self {
        let mut users: Vec<String> = Vec::new();
        for tok in expr
            .split([',', '|'])
            .flat_map(|chunk| {
                let t = chunk.trim();
                // Keep "First Last" together when separated by commas; otherwise split words.
                if expr.contains(',') || expr.contains('|') {
                    vec![t]
                } else {
                    t.split_whitespace().collect()
                }
            })
            .filter(|t| !t.is_empty())
        {
            if !users.iter().any(|u| u == tok) {
                users.push(tok.to_string());
            }
        }
        Self { users }
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    pub fn label(&self) -> String {
        match self.users.len() {
            0 => "All".to_string(),
            1 | 2 => self.users.join(", "),
            n => format!("{} +{}", self.users[0], n - 1),
        }
    }

    fn is_me(tok: &str) -> bool {
        tok.eq_ignore_ascii_case("@me") || tok.eq_ignore_ascii_case("me")
    }

    /// True when this user filter includes `@me` / `me` (used to auto-suppress
    /// `My Commits` bold highlighting when the entire table is already filtered to `@me`).
    pub fn has_me(&self) -> bool {
        self.users.iter().any(|tok| Self::is_me(tok))
    }

    pub fn matches(&self, name: &str, email: &str, me: RepoUser<'_>) -> bool {
        self.matcher().matches(name, email, me)
    }

    /// Pre-normalised form for matching many commits: query tokens are
    /// lowercased once instead of once per commit.
    pub fn matcher(&self) -> UserMatcher {
        UserMatcher {
            tokens: self
                .users
                .iter()
                .map(|tok| {
                    if Self::is_me(tok) {
                        UserToken::Me
                    } else {
                        UserToken::Fragment(tok.to_lowercase())
                    }
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UserToken {
    Me,
    /// Lowercased name / email fragment.
    Fragment(String),
}

/// Compiled [`UserFilter`]; see [`UserFilter::matcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMatcher {
    tokens: Vec<UserToken>,
}

impl UserMatcher {
    pub fn matches(&self, name: &str, email: &str, me: RepoUser<'_>) -> bool {
        self.tokens.iter().any(|tok| match tok {
            UserToken::Me => {
                me.email.is_some_and(|e| e.eq_ignore_ascii_case(email))
                    || me.name.is_some_and(|n| n == name)
            }
            UserToken::Fragment(t) => contains_lowercased(name, t) || contains_lowercased(email, t),
        })
    }
}

/// `haystack.to_lowercase().contains(needle_lower)` without allocating on the
/// common all-ASCII path.
fn contains_lowercased(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle_lower.as_bytes());
        return h.len() >= n.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle_lower)
}

/// Commit-date window (`today`, `yesterday`, `7d`, `30d`, `2026-01-01..2026-02-01`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DateFilter {
    /// Inclusive lower bound (unix seconds).
    pub since: Option<i64>,
    /// Exclusive upper bound (unix seconds).
    pub until: Option<i64>,
    pub label: String,
}

impl DateFilter {
    pub fn is_empty(&self) -> bool {
        self.since.is_none() && self.until.is_none()
    }

    pub fn matches(&self, ts: i64) -> bool {
        self.since.is_none_or(|s| ts >= s) && self.until.is_none_or(|u| ts < u)
    }

    /// Parses a preset or `YYYY-MM-DD[..YYYY-MM-DD]` range relative to `now` (unix seconds).
    /// Returns `None` for unparseable input; empty input yields an empty filter.
    pub fn parse(expr: &str, now: i64) -> Option<Self> {
        let e = expr.trim().to_ascii_lowercase();
        if e.is_empty() || e == "all" || e == "any" {
            return Some(Self::default());
        }
        let now_dt = Local.timestamp_opt(now, 0).single()?;
        let today = now_dt.date_naive();
        let start_of = |d: NaiveDate| -> Option<i64> {
            Local
                .from_local_datetime(&d.and_hms_opt(0, 0, 0)?)
                .earliest()
                .map(|dt| dt.timestamp())
        };
        let preset = |since: Option<i64>, until: Option<i64>, label: &str| {
            Some(Self {
                since,
                until,
                label: label.to_string(),
            })
        };
        match e.as_str() {
            "today" => return preset(start_of(today), None, "Today"),
            "yesterday" => {
                let y = today.pred_opt()?;
                return preset(start_of(y), start_of(today), "Yesterday");
            }
            "7d" | "week" | "last 7 days" => {
                return preset(Some(now.checked_sub(7 * 86_400)?), None, "Last 7 Days");
            }
            "30d" | "month" | "last 30 days" => {
                return preset(Some(now.checked_sub(30 * 86_400)?), None, "Last 30 Days");
            }
            _ => {}
        }
        if let Some(n) = e
            .strip_suffix('d')
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|&n| n > 0)
        {
            let secs = n.checked_mul(86_400).and_then(|s| i64::try_from(s).ok())?;
            let since = now.checked_sub(secs)?;
            return preset(Some(since), None, &format!("Last {n} Days"));
        }
        let parse_day = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
        if let Some((a, b)) = e.split_once("..") {
            let since = if a.trim().is_empty() {
                None
            } else {
                Some(start_of(parse_day(a)?)?)
            };
            let until = if b.trim().is_empty() {
                None
            } else {
                Some(start_of(parse_day(b)?.succ_opt()?)?)
            };
            return Some(Self {
                since,
                until,
                label: format!("{}..{}", a.trim(), b.trim()),
            });
        }
        let day = parse_day(&e)?;
        let next_day = day.succ_opt()?;
        preset(
            start_of(day),
            start_of(next_day),
            &day.format("%Y-%m-%d").to_string(),
        )
    }
}

/// Repository-relative path constraints (`--full-history --simplify-merges`
/// semantics are applied by the engine that computes path matches).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathFilter {
    pub paths: Vec<String>,
}

impl PathFilter {
    pub fn single(path: &str) -> Self {
        let norm = path
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if norm.is_empty() {
            Self::default()
        } else {
            Self { paths: vec![norm] }
        }
    }

    pub fn parse(expr: &str) -> Self {
        let mut paths: Vec<String> = Vec::new();
        for tok in expr
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
        {
            let norm = tok.replace('\\', "/");
            let norm = norm
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string();
            if !norm.is_empty() && !paths.contains(&norm) {
                paths.push(norm);
            }
        }
        Self { paths }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn label(&self) -> String {
        match self.paths.len() {
            0 => "All".to_string(),
            1 => self.paths[0].clone(),
            n => format!("{} +{}", self.paths[0], n - 1),
        }
    }

    /// True when `path` hits any configured path filter (exact file, directory
    /// prefix with `/` boundary, or glob pattern). Empty filter matches all.
    pub fn matches_path(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        let owned;
        let norm = if path.contains('\\') {
            owned = path.replace('\\', "/");
            owned.trim_start_matches("./").trim_end_matches('/')
        } else {
            path.trim_start_matches("./").trim_end_matches('/')
        };
        self.paths.iter().any(|fp| {
            norm == fp
                || (norm.starts_with(fp.as_str()) && norm.as_bytes().get(fp.len()) == Some(&b'/'))
                || ((fp.contains('*') || fp.contains('?')) && glob_match(fp, norm))
        })
    }

    /// True when either the post-change `path` or pre-rename `old_path` hits
    /// this filter (aligned with IDEA `VcsLogAsyncChangesTreeModel.collectAffectedChanges`).
    pub fn matches_change(&self, path: &str, old_path: Option<&str>) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        self.matches_path(path) || old_path.is_some_and(|old| self.matches_path(old))
    }
}

/// Read-only commit-set constraint (Update Info "View", `GitUpdateInfoAsLog`):
/// only the listed commits match. Set programmatically, cleared from its chip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeFilter {
    /// Chip label, e.g. `abc1234..def5678`.
    pub label: String,
    /// Sorted, de-duplicated commit ids.
    oids: Vec<Oid>,
}

impl RangeFilter {
    pub fn new(label: impl Into<String>, oids: impl IntoIterator<Item = Oid>) -> Self {
        let mut oids: Vec<Oid> = oids.into_iter().collect();
        oids.sort_unstable();
        oids.dedup();
        Self {
            label: label.into(),
            oids,
        }
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.oids.binary_search(oid).is_ok()
    }

    pub fn len(&self) -> usize {
        self.oids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.oids.is_empty()
    }
}

/// Full filter collection; empty dimensions impose no constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilterSpec {
    pub text: TextFilter,
    pub branch: BranchFilter,
    pub user: UserFilter,
    pub date: DateFilter,
    pub paths: PathFilter,
    /// Active read-only commit range (an empty range matches nothing).
    pub range: Option<RangeFilter>,
    /// Exclude merge commits (`parents.len() > 1`, `--no-merges`).
    pub no_merges: bool,
}

impl LogFilterSpec {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.branch.is_empty()
            && self.user.is_empty()
            && self.date.is_empty()
            && self.paths.is_empty()
            && self.range.is_none()
            && !self.no_merges
    }

    /// True when only `BranchFilter` is active (no text, user, date, paths,
    /// range, or `--no-merges` constraint), allowing linear-branch collapsing.
    pub fn is_branch_only(&self) -> bool {
        !self.branch.is_empty()
            && self.text.is_empty()
            && self.user.is_empty()
            && self.date.is_empty()
            && self.paths.is_empty()
            && self.range.is_none()
            && !self.no_merges
    }

    /// Computes the matching-node bitset over `graph` (indexed by `PermNodeId`).
    ///
    /// Returns `Ok(None)` when no dimension constrains the result. When a path
    /// filter is set but `path_matches` is not yet available, the path
    /// dimension is skipped (results are refined once the engine reports).
    pub fn compute_matches(
        &self,
        graph: &PermanentLinearGraph,
        snapshot: &RepoSnapshot,
        path_matches: Option<&HashSet<Oid>>,
    ) -> Result<Option<FixedBitSet>, FilterError> {
        self.compute_matches_cancellable(graph, snapshot, path_matches, || false)
            .expect("uncancellable match run")
    }

    /// Evaluates active filter dimensions against `graph`, returning `None` if
    /// `is_cancelled()` becomes true mid-walk.
    pub fn compute_matches_cancellable(
        &self,
        graph: &PermanentLinearGraph,
        snapshot: &RepoSnapshot,
        path_matches: Option<&HashSet<Oid>>,
        is_cancelled: impl Fn() -> bool,
    ) -> Option<Result<Option<FixedBitSet>, FilterError>> {
        self.compute_matches_with_favorites(graph, snapshot, path_matches, &[], is_cancelled)
            .map(|r| r.map(|(bits, _)| bits))
    }

    /// Evaluates active filter dimensions with favorite branch expansion and
    /// returns `(matching_bitset, branch_start_heads)` so `FirstParentOnly` can
    /// walk exclusively from the active branch heads.
    pub fn compute_matches_with_favorites(
        &self,
        graph: &PermanentLinearGraph,
        snapshot: &RepoSnapshot,
        path_matches: Option<&HashSet<Oid>>,
        favorites: &[String],
        is_cancelled: impl Fn() -> bool,
    ) -> Option<Result<FilterMatchWithHeads, FilterError>> {
        if is_cancelled() {
            return None;
        }
        let text = match self.text.compile() {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };
        let use_paths = !self.paths.is_empty() && path_matches.is_some();
        let range = self.range.as_ref();
        if self.branch.is_empty()
            && text.is_none()
            && self.user.is_empty()
            && self.date.is_empty()
            && !use_paths
            && range.is_none()
            && !self.no_merges
        {
            return Some(Ok((None, None)));
        }

        let n = graph.nodes_count as usize;
        let (mut matches, branch_heads) = if self.branch.is_empty() {
            let mut all = FixedBitSet::with_capacity(n);
            all.insert_range(..);
            (all, None)
        } else {
            let (mask, heads) = self
                .branch
                .compute_mask_and_heads(graph, snapshot, favorites);
            (mask, Some(heads))
        };
        if is_cancelled() {
            return None;
        }

        if text.is_none()
            && self.user.is_empty()
            && self.date.is_empty()
            && !use_paths
            && range.is_none()
            && !self.no_merges
        {
            return Some(Ok((Some(matches), branch_heads)));
        }

        let me = RepoUser {
            name: snapshot.user_name.as_deref(),
            email: snapshot.user_email.as_deref(),
        };
        let user = (!self.user.is_empty()).then(|| self.user.matcher());
        for node in 0..n {
            if node.is_multiple_of(1024) && is_cancelled() {
                return None;
            }
            if !matches.contains(node) {
                continue;
            }
            let Some(commit) = graph
                .perm_to_input
                .get(node)
                .and_then(|&i| snapshot.commits.get(i))
            else {
                matches.set(node, false);
                continue;
            };
            let ok = (!self.no_merges || commit.parents.len() <= 1)
                && text
                    .as_ref()
                    .is_none_or(|m| m.matches_commit(&commit.oid, &commit.subject, &commit.body))
                && user
                    .as_ref()
                    .is_none_or(|u| u.matches(&commit.author_name, &commit.author_email, me))
                && self.date.matches(commit.timestamp)
                && (!use_paths || path_matches.is_some_and(|set| set.contains(&commit.oid)))
                && range.is_none_or(|r| r.contains(&commit.oid));
            if !ok {
                matches.set(node, false);
            }
        }
        Some(Ok((Some(matches), branch_heads)))
    }
}

/// Glob match supporting `*` (any run, including `/`) and `?` (one char).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod user_matcher_tests {
    use super::*;

    #[test]
    fn user_matcher_is_case_insensitive_for_ascii_and_unicode() {
        let me = RepoUser {
            name: Some("Me Myself"),
            email: Some("me@example.com"),
        };
        let f = UserFilter::parse("ALICE");
        assert!(f.matches("alice smith", "a@x.io", me));
        assert!(f.matches("Bob", "Alice@Example.com", me));
        assert!(!f.matches("Bob", "bob@x.io", me));

        let f = UserFilter::parse("ÉLODIE");
        assert!(f.matches("élodie Martin", "e@x.fr", me));
        assert!(!f.matches("Elodie", "e@x.fr", me));

        let f = UserFilter::parse("@me");
        assert!(f.matches("Someone", "ME@example.com", me));
        assert!(f.matches("Me Myself", "other@x.io", me));
        assert!(!f.matches("Someone", "other@x.io", me));
    }

    #[test]
    fn contains_lowercased_matches_reference_semantics() {
        for (hay, needle) in [
            ("Hello World", "world"),
            ("Hello World", "hello w"),
            ("abc", "abcd"),
            ("", "a"),
            ("ÀB", "àb"),
            ("x", ""),
        ] {
            let needle_lower = needle.to_lowercase();
            assert_eq!(
                contains_lowercased(hay, &needle_lower),
                hay.to_lowercase().contains(&needle_lower),
                "{hay:?} / {needle:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{PermanentLinearGraphBuilder, RawCommitNode};
    use crate::{CommitSummary, RefKind};

    fn oid(n: u8) -> Oid {
        Oid::from_bytes([n; 20])
    }

    fn commit(n: u8, parents: &[u8], author: &str, ts: i64, subject: &str) -> CommitSummary {
        CommitSummary {
            oid: oid(n),
            parents: parents.iter().map(|&p| oid(p)).collect(),
            author_name: author.to_string(),
            author_email: format!("{}@x.io", author.to_lowercase()),
            timestamp: ts,
            author_timestamp: ts,
            subject: subject.to_string(),
            body: String::new(),
        }
    }

    fn branch(name: &str, target: u8) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/heads/{name}"),
            target_oid: oid(target),
            kind: RefKind::LocalBranch,
            upstream: None,
            ahead_behind: None,
            is_head: false,
        }
    }

    /// 1 <- 2 <- 3 (main); 1 <- 4 <- 5 (feature/a); 1 <- 6 (dependabot/x)
    fn fixture() -> (RepoSnapshot, PermanentLinearGraph) {
        let commits = vec![
            commit(6, &[1], "Bot", 60, "bump deps"),
            commit(5, &[4], "Alice", 50, "feat: login page"),
            commit(3, &[2], "Bob", 30, "fix: crash in parser"),
            commit(4, &[1], "Alice", 40, "feat: oauth"),
            commit(2, &[1], "Bob", 20, "refactor graph"),
            commit(1, &[], "Alice", 10, "initial"),
        ];
        let snap = RepoSnapshot {
            refs: vec![
                branch("main", 3),
                branch("feature/a", 5),
                branch("dependabot/x", 6),
            ],
            head_oid: Some(oid(3)),
            commits: commits.clone(),
            user_name: Some("Alice".into()),
            user_email: Some("alice@x.io".into()),
            ..Default::default()
        };
        let raw: Vec<RawCommitNode> = commits.iter().map(RawCommitNode::from).collect();
        (snap, PermanentLinearGraphBuilder::build(&raw))
    }

    fn matched(spec: &LogFilterSpec) -> Vec<u8> {
        let (snap, graph) = fixture();
        let Some(bits) = spec.compute_matches(&graph, &snap, None).unwrap() else {
            return vec![];
        };
        let mut out: Vec<u8> = bits
            .ones()
            .map(|node| graph.commit(node as u32).oid.as_bytes()[0])
            .collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn range_filter_keeps_only_listed_commits_and_composes() {
        let spec = LogFilterSpec {
            range: Some(RangeFilter::new("x..y", [oid(5), oid(4), oid(5)])),
            ..Default::default()
        };
        assert!(!spec.is_empty());
        assert_eq!(spec.range.as_ref().unwrap().len(), 2);
        assert_eq!(matched(&spec), vec![4, 5]);

        let empty = LogFilterSpec {
            range: Some(RangeFilter::new("x..x", [])),
            ..Default::default()
        };
        assert!(matched(&empty).is_empty());

        let both = LogFilterSpec {
            range: Some(RangeFilter::new("r", [oid(4), oid(5), oid(6)])),
            user: UserFilter::parse("alice"),
            ..Default::default()
        };
        let got = matched(&both);
        assert!(got.iter().all(|n| [4, 5, 6].contains(n)));
        assert!(!got.contains(&6), "Bot commit filtered by user");
    }

    #[test]
    fn branch_filter_union_exclusion_and_globs() {
        let spec = LogFilterSpec {
            branch: BranchFilter::parse("feature/*"),
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![1, 4, 5]);

        let spec = LogFilterSpec {
            branch: BranchFilter::from_names(vec!["main".into(), "feature/a".into()]),
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![1, 2, 3, 4, 5]);

        let spec = LogFilterSpec {
            branch: BranchFilter::parse("-dependabot/*"),
            ..Default::default()
        };
        // HEAD (3) + main + feature/a remain; dependabot-only commit 6 is hidden.
        assert_eq!(matched(&spec), vec![1, 2, 3, 4, 5]);
        assert_eq!(
            BranchFilter::parse("main +feat/* -dependabot/*").label(),
            "main + feat/* -dependabot/*"
        );

        // Even when HEAD points to an excluded branch, `-dependabot/*` excludes it.
        let (mut snap, graph) = fixture();
        snap.head_branch = Some("dependabot/x".into());
        snap.head_oid = Some(oid(6));
        let bits = spec.compute_matches(&graph, &snap, None).unwrap().unwrap();
        let mut got: Vec<u8> = bits
            .ones()
            .map(|n| graph.commit(n as u32).oid.as_bytes()[0])
            .collect();
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn text_regex_case_hash_user_and_date_intersect() {
        let spec = LogFilterSpec {
            text: TextFilter {
                query: "FEAT".into(),
                regex: false,
                match_case: false,
            },
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![4, 5]);

        let spec = LogFilterSpec {
            text: TextFilter {
                query: "FEAT".into(),
                regex: false,
                match_case: true,
            },
            ..Default::default()
        };
        assert!(matched(&spec).is_empty());

        let spec = LogFilterSpec {
            text: TextFilter {
                query: r"^(fix|refactor)\b".into(),
                regex: true,
                match_case: false,
            },
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![2, 3]);

        // Hash prefix: oid(5) hex starts with "0505".
        let spec = LogFilterSpec {
            text: TextFilter {
                query: "0505".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![5]);

        let spec = LogFilterSpec {
            user: UserFilter::parse("@me"),
            date: DateFilter {
                since: Some(15),
                until: None,
                label: String::new(),
            },
            ..Default::default()
        };
        assert_eq!(matched(&spec), vec![4, 5]);

        let bad = LogFilterSpec {
            text: TextFilter {
                query: "(".into(),
                regex: true,
                match_case: false,
            },
            ..Default::default()
        };
        let (snap, graph) = fixture();
        assert!(bad.compute_matches(&graph, &snap, None).is_err());
    }

    #[test]
    fn path_matches_apply_only_when_available() {
        let (snap, graph) = fixture();
        let spec = LogFilterSpec {
            paths: PathFilter::parse("./src/graph/ docs"),
            ..Default::default()
        };
        assert_eq!(spec.paths.paths, vec!["src/graph", "docs"]);
        assert!(spec.compute_matches(&graph, &snap, None).unwrap().is_none());
        let set: HashSet<Oid> = [oid(2), oid(3)].into_iter().collect();
        let bits = spec
            .compute_matches(&graph, &snap, Some(&set))
            .unwrap()
            .unwrap();
        assert_eq!(bits.count_ones(..), 2);
    }

    #[test]
    fn path_filter_single_preserves_spaces_and_normalizes() {
        let pf = PathFilter::single("dir with space/a.rs");
        assert_eq!(pf.paths, vec!["dir with space/a.rs".to_string()]);
        let pf2 = PathFilter::single("./sub dir/中文.txt/");
        assert_eq!(pf2.paths, vec!["sub dir/中文.txt".to_string()]);
        assert!(PathFilter::single("").is_empty());
        assert!(PathFilter::single("./").is_empty());
    }

    #[test]
    fn path_filter_matches_exact_dir_prefix_renames_and_globs() {
        let empty = PathFilter::default();
        assert!(empty.matches_change("anything.rs", None));

        let file = PathFilter::single("src/a.rs");
        assert!(file.matches_change("src/a.rs", None));
        assert!(file.matches_change("./src\\a.rs", None));
        assert!(!file.matches_change("src/a.rs.bak", None));
        assert!(!file.matches_change("src/b.rs", None));
        // Rename touching either old or new path matches.
        assert!(file.matches_change("src/renamed.rs", Some("src/a.rs")));
        assert!(file.matches_change("src/a.rs", Some("src/old.rs")));

        let dir = PathFilter::parse("src/sub docs/");
        assert!(dir.matches_change("src/sub/mod.rs", None));
        assert!(dir.matches_change("src/sub/deep/file.rs", None));
        assert!(dir.matches_change("docs/manual.md", None));
        assert!(!dir.matches_change("src/sub2/mod.rs", None));
        assert!(!dir.matches_change("src/other.rs", None));

        let glob = PathFilter::parse("src/*.rs");
        assert!(glob.matches_change("src/a.rs", None));
        assert!(!glob.matches_change("docs/a.md", None));
    }

    #[test]
    fn date_presets_and_ranges_parse() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 18, 14, 20, 0)
            .unwrap()
            .timestamp();
        let today = DateFilter::parse("today", now).unwrap();
        assert!(today.matches(now) && !today.matches(now - 86_400));
        let y = DateFilter::parse("yesterday", now).unwrap();
        assert!(y.matches(now - 86_400) && !y.matches(now));
        let range = DateFilter::parse("2026-11-01..2026-11-17", now).unwrap();
        assert!(range.matches(now - 2 * 86_400) && !range.matches(now));
        assert!(DateFilter::parse("garbage", now).is_none());
        assert!(DateFilter::parse("", now).unwrap().is_empty());
        assert!(glob_match("feat/*", "feat/a/b") && !glob_match("feat/?", "feat/ab"));
    }

    #[test]
    fn multi_hash_space_separated_matches_any_commit() {
        let (snap, graph) = fixture();
        let h1 = oid(1).short();
        let h3 = oid(3).short();
        let spec = LogFilterSpec {
            text: TextFilter {
                query: format!("{h1}  {h3}"),
                regex: false,
                match_case: false,
            },
            ..Default::default()
        };
        let bits = spec.compute_matches(&graph, &snap, None).unwrap().unwrap();
        assert_eq!(bits.count_ones(..), 2);
        assert!(bits.contains(graph.node_of_oid(&oid(1)).unwrap() as usize));
        assert!(bits.contains(graph.node_of_oid(&oid(3)).unwrap() as usize));
        assert!(!bits.contains(graph.node_of_oid(&oid(2)).unwrap() as usize));
    }

    #[test]
    fn branch_range_and_favorites_and_no_merges_filter() {
        let (mut snap, _) = fixture();
        // Add a merge commit 7 merging main (3) and feature/a (5) for --no-merges testing.
        snap.commits
            .insert(0, commit(7, &[3, 5], "Alice", 70, "merge feature/a"));
        snap.refs.push(branch("merged", 7));
        let raw: Vec<RawCommitNode> = snap.commits.iter().map(RawCommitNode::from).collect();
        let graph = PermanentLinearGraphBuilder::build(&raw);

        // fixture: main -> 3 -> 2 -> 1; feature/a -> 5 -> 4 -> 1
        let bf = BranchFilter::parse("main..feature/a");
        assert_eq!(
            bf.ranges,
            vec![("main".to_string(), "feature/a".to_string())]
        );
        assert_eq!(bf.label(), "main..feature/a");
        assert_eq!(bf.single_compared_branch(Some("main")), Some("feature/a"));

        // main..feature/a: reachable from feature/a (5,4,1) excluding main (3,2,1) = {4, 5}.
        let spec = LogFilterSpec {
            branch: bf,
            ..Default::default()
        };
        assert!(spec.is_branch_only());
        let (bits, heads) = spec
            .compute_matches_with_favorites(&graph, &snap, None, &[], || false)
            .unwrap()
            .unwrap();
        let bits = bits.unwrap();
        assert_eq!(bits.count_ones(..), 2);
        assert!(bits.contains(graph.node_of_oid(&oid(4)).unwrap() as usize));
        assert!(bits.contains(graph.node_of_oid(&oid(5)).unwrap() as usize));
        assert_eq!(heads.unwrap(), vec![graph.node_of_oid(&oid(5)).unwrap()]);

        // Favorites expands to favorite branches list (`feature/a` -> {1, 4, 5}).
        let fav_spec = LogFilterSpec {
            branch: BranchFilter::parse("Favorites"),
            ..Default::default()
        };
        let (fav_bits, _) = fav_spec
            .compute_matches_with_favorites(&graph, &snap, None, &["feature/a".to_string()], || {
                false
            })
            .unwrap()
            .unwrap();
        let fav_bits = fav_bits.unwrap();
        assert_eq!(fav_bits.count_ones(..), 3);
        assert!(fav_bits.contains(graph.node_of_oid(&oid(1)).unwrap() as usize));
        assert!(fav_bits.contains(graph.node_of_oid(&oid(4)).unwrap() as usize));
        assert!(fav_bits.contains(graph.node_of_oid(&oid(5)).unwrap() as usize));

        // --no-merges excludes merge commit 7 (parents = [3, 5]).
        let nm_spec = LogFilterSpec {
            no_merges: true,
            ..Default::default()
        };
        assert!(!nm_spec.is_empty());
        assert!(!nm_spec.is_branch_only());
        let nm_bits = nm_spec
            .compute_matches(&graph, &snap, None)
            .unwrap()
            .unwrap();
        assert!(!nm_bits.contains(graph.node_of_oid(&oid(7)).unwrap() as usize));
        assert_eq!(nm_bits.count_ones(..), 6);

        // CurrentBranch auto-suppression & cherry-picked branch detection.
        assert!(BranchFilter::parse("HEAD").is_only_current_branch(Some("main")));
        assert!(BranchFilter::parse("main").is_only_current_branch(Some("main")));
        assert!(!BranchFilter::parse("feature/a").is_only_current_branch(Some("main")));
        assert!(!BranchFilter::parse("main feature/a").is_only_current_branch(Some("main")));
        assert_eq!(
            BranchFilter::parse("feature/a").single_compared_branch(Some("main")),
            Some("feature/a")
        );
        assert_eq!(
            BranchFilter::parse("feature/a main").single_compared_branch(Some("main")),
            Some("feature/a")
        );
        assert_eq!(
            BranchFilter::parse("main").single_compared_branch(Some("main")),
            None
        );

        // Typo in `from` of `from..to` must NOT widen to all commits reachable from `to`.
        let bad_range = LogFilterSpec {
            branch: BranchFilter::parse("nonexistent_branch..main"),
            ..Default::default()
        };
        assert!(matched(&bad_range).is_empty());

        // Exclusions `-branch` must subtract from `a..b` ranges too.
        let range_with_excl = LogFilterSpec {
            branch: BranchFilter::parse("feature/a..main -main"),
            ..Default::default()
        };
        assert!(matched(&range_with_excl).is_empty());

        // Range exclusion `-HEAD` must work even on detached HEAD (not in `snapshot.refs`).
        let detached_snap = RepoSnapshot {
            head_oid: Some(oid(3)),
            head_branch: None,
            ..snap.clone()
        };
        let range_excl_head = BranchFilter::parse("feature/a..merged -HEAD");
        let (bits, _) = range_excl_head.compute_mask_and_heads(&graph, &detached_snap, &[]);
        // Commits reachable from merged (7) but not feature/a (5) are {2, 3, 7};
        // subtracting detached HEAD at 3 removes {1, 2, 3}, leaving {7}.
        assert!(bits.contains(graph.node_of_oid(&oid(7)).unwrap() as usize));
        assert!(!bits.contains(graph.node_of_oid(&oid(3)).unwrap() as usize));
        assert!(!bits.contains(graph.node_of_oid(&oid(2)).unwrap() as usize));
        assert_eq!(bits.count_ones(..), 1);
    }

    #[test]
    fn date_filter_parse_handles_overflow_negative_days_and_max_date_without_panicking() {
        let now = 1_775_000_000i64;
        assert_eq!(DateFilter::parse("+262142-12-31", now), None);
        assert_eq!(DateFilter::parse("..+262142-12-31", now), None);
        assert_eq!(DateFilter::parse("999999999999999d", now), None);
        assert_eq!(DateFilter::parse("-7d", now), None);
        assert_eq!(DateFilter::parse("0d", now), None);
        let valid = DateFilter::parse("14d", now).unwrap();
        assert_eq!(valid.since, Some(now - 14 * 86_400));
        assert_eq!(valid.label, "Last 14 Days");
    }
}
