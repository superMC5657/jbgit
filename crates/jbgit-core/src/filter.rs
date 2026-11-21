//! Multi-dimensional log filter (`VcsLogFilterCollection` in IDEA):
//! text / hash, branch, user, date and path constraints intersected into a
//! single matching-node bitset for [`crate::graph::VisibleGraph::build`].

use std::collections::HashSet;

use chrono::{Duration, Local, NaiveDate, TimeZone};
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
    hash_prefix: Option<String>,
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
        let hash_prefix =
            (q.len() >= 4 && q.len() <= 40 && q.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| q.to_ascii_lowercase());
        Ok(Some(TextMatcher { re, hash_prefix }))
    }
}

impl TextMatcher {
    pub fn matches_commit(&self, oid: &Oid, subject: &str, body: &str) -> bool {
        self.hash_prefix
            .as_deref()
            .is_some_and(|p| oid.matches_hex_prefix(p))
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

/// Branch include / exclude rules (`main feat/* -dependabot/*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
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
        }
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    pub fn to_expr(&self) -> String {
        self.include
            .iter()
            .cloned()
            .chain(self.exclude.iter().map(|e| format!("-{e}")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Compact chip label: `main + feat/*`, `main +2`, `-dependabot/*`.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        match self.include.len() {
            0 => {}
            1 | 2 => parts.push(self.include.join(" + ")),
            n => parts.push(format!("{} +{}", self.include[0], n - 1)),
        }
        parts.extend(self.exclude.iter().map(|e| format!("-{e}")));
        if parts.is_empty() {
            "All".to_string()
        } else {
            parts.join(" ")
        }
    }

    fn ref_matches(pattern: &str, r: &RefInfo) -> bool {
        glob_match(pattern, &r.name) || glob_match(pattern, &r.full_name)
    }

    /// Head commits whose ancestry forms the visible set.
    pub fn select_heads(
        &self,
        refs: &[RefInfo],
        head_branch: Option<&str>,
        head_oid: Option<Oid>,
    ) -> Vec<Oid> {
        let mut out = Vec::new();
        let include_all = self.include.is_empty();
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
            UserToken::Fragment(t) => {
                contains_lowercased(name, t) || contains_lowercased(email, t)
            }
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
        return h.len() >= n.len()
            && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
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
                let y = today - Duration::days(1);
                return preset(start_of(y), start_of(today), "Yesterday");
            }
            "7d" | "week" | "last 7 days" => {
                return preset(Some(now - 7 * 86_400), None, "Last 7 Days");
            }
            "30d" | "month" | "last 30 days" => {
                return preset(Some(now - 30 * 86_400), None, "Last 30 Days");
            }
            _ => {}
        }
        if let Some(n) = e.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
            return preset(Some(now - n * 86_400), None, &format!("Last {n} Days"));
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
                Some(start_of(parse_day(b)? + Duration::days(1))?)
            };
            return Some(Self {
                since,
                until,
                label: format!("{}..{}", a.trim(), b.trim()),
            });
        }
        let day = parse_day(&e)?;
        preset(
            start_of(day),
            start_of(day + Duration::days(1)),
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
}

/// Full filter collection; empty dimensions impose no constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilterSpec {
    pub text: TextFilter,
    pub branch: BranchFilter,
    pub user: UserFilter,
    pub date: DateFilter,
    pub paths: PathFilter,
}

impl LogFilterSpec {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.branch.is_empty()
            && self.user.is_empty()
            && self.date.is_empty()
            && self.paths.is_empty()
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
        if is_cancelled() {
            return None;
        }
        let text = match self.text.compile() {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };
        let use_paths = !self.paths.is_empty() && path_matches.is_some();
        if self.branch.is_empty()
            && text.is_none()
            && self.user.is_empty()
            && self.date.is_empty()
            && !use_paths
        {
            return Some(Ok(None));
        }

        let n = graph.nodes_count as usize;
        let mut matches = if self.branch.is_empty() {
            let mut all = FixedBitSet::with_capacity(n);
            all.insert_range(..);
            all
        } else {
            let heads: Vec<_> = self
                .branch
                .select_heads(
                    &snapshot.refs,
                    snapshot.head_branch.as_deref(),
                    snapshot.head_oid,
                )
                .iter()
                .filter_map(|oid| graph.node_of_oid(oid))
                .collect();
            ReachableNodes::contained_in_branch(graph, &heads)
        };
        if is_cancelled() {
            return None;
        }

        if text.is_none() && self.user.is_empty() && self.date.is_empty() && !use_paths {
            return Some(Ok(Some(matches)));
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
            let ok = text
                .as_ref()
                .is_none_or(|m| m.matches_commit(&commit.oid, &commit.subject, &commit.body))
                && user
                    .as_ref()
                    .is_none_or(|u| u.matches(&commit.author_name, &commit.author_email, me))
                && self.date.matches(commit.timestamp)
                && (!use_paths || path_matches.is_some_and(|set| set.contains(&commit.oid)));
            if !ok {
                matches.set(node, false);
            }
        }
        Some(Ok(Some(matches)))
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
}
