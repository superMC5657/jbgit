/// Parses `.git/logs/HEAD` (newest entries last) and returns up to `limit`
/// unique local branches the user checked out recently (newest first).
pub fn parse_recent_checkouts(
    reflog: &str,
    current: Option<&str>,
    existing: &[&str],
    scan: usize,
    limit: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in reflog.lines().rev().take(scan) {
        let Some((_, msg)) = line.split_once('\t') else {
            continue;
        };
        let Some(rest) = msg.strip_prefix("checkout: moving from ") else {
            continue;
        };
        let Some((from, to)) = rest.rsplit_once(" to ") else {
            continue;
        };
        for name in [to.trim(), from.trim()] {
            if name.is_empty()
                || Some(name) == current
                || out.iter().any(|n| n == name)
                || !existing.contains(&name)
            {
                continue;
            }
            out.push(name.to_string());
        }
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

/// Typed reason a branch or tag name fails `git check-ref-format`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefNameError {
    Empty,
    StartsWithDash,
    ContainsSpaces,
    ContainsDoubleDot,
    ContainsAtBrace,
    InvalidChar(char),
    EmptyPathComponent,
    EndsWithDot,
    ComponentStartsWithDot(String),
    ComponentEndsWithLock(String),
    ReservedHead,
    AlreadyExists(String),
    ConflictsWithExisting(String),
}

impl RefNameError {
    pub fn format_en(&self, what: &str) -> String {
        match self {
            Self::Empty => format!("{what} name cannot be empty"),
            Self::StartsWithDash => format!("{what} name cannot start with '-'"),
            Self::ContainsSpaces => format!("{what} name cannot contain spaces"),
            Self::ContainsDoubleDot => format!("{what} name cannot contain '..'"),
            Self::ContainsAtBrace => format!("{what} name cannot contain '@{{' or be '@'"),
            Self::InvalidChar(c) => format!("{what} name cannot contain '{c}'"),
            Self::EmptyPathComponent => format!("{what} name has an empty path component"),
            Self::EndsWithDot => format!("{what} name cannot end with '.'"),
            Self::ComponentStartsWithDot(comp) => {
                format!("path component cannot start with '.' ('{comp}')")
            }
            Self::ComponentEndsWithLock(comp) => {
                format!("path component cannot end with '.lock' ('{comp}')")
            }
            Self::ReservedHead => format!("'HEAD' is not a valid {} name", what.to_lowercase()),
            Self::AlreadyExists(name) => format!("{what} '{name}' already exists"),
            Self::ConflictsWithExisting(conflict) => {
                format!("{what} name conflicts with existing '{conflict}'")
            }
        }
    }
}

/// Typed `git check-ref-format --branch` validation plus duplicate detection.
pub fn validate_ref_name_typed(name: &str, existing: &[&str]) -> Result<(), RefNameError> {
    if name.is_empty() {
        return Err(RefNameError::Empty);
    }
    if name.starts_with('-') {
        return Err(RefNameError::StartsWithDash);
    }
    if name.chars().any(|c| c.is_whitespace()) {
        return Err(RefNameError::ContainsSpaces);
    }
    if name.contains("..") {
        return Err(RefNameError::ContainsDoubleDot);
    }
    if name.contains("@{") || name == "@" {
        return Err(RefNameError::ContainsAtBrace);
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_control() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err(RefNameError::InvalidChar(c));
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err(RefNameError::EmptyPathComponent);
    }
    if name.ends_with('.') {
        return Err(RefNameError::EndsWithDot);
    }
    for comp in name.split('/') {
        if comp.starts_with('.') {
            return Err(RefNameError::ComponentStartsWithDot(comp.to_string()));
        }
        if comp.ends_with(".lock") {
            return Err(RefNameError::ComponentEndsWithLock(comp.to_string()));
        }
    }
    if name == "HEAD" {
        return Err(RefNameError::ReservedHead);
    }
    if existing.contains(&name) {
        return Err(RefNameError::AlreadyExists(name.to_string()));
    }
    if let Some(conflict) = existing.iter().find(|e| {
        e.strip_prefix(name).is_some_and(|r| r.starts_with('/'))
            || name.strip_prefix(**e).is_some_and(|r| r.starts_with('/'))
    }) {
        return Err(RefNameError::ConflictsWithExisting((*conflict).to_string()));
    }
    Ok(())
}

/// `git check-ref-format --branch` rules plus duplicate detection.
pub fn validate_ref_name(name: &str, existing: &[&str], what: &str) -> Result<(), String> {
    validate_ref_name_typed(name, existing).map_err(|err| err.format_en(what))
}

/// Fuzzy (`nucleo`) match returning the score and the matched **char**
/// indices of `haystack` for highlighting. `None` when it does not match.
pub fn fuzzy_indices(haystack: &str, needle: &str) -> Option<(u32, Vec<u32>)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths());
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        needle,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let score = pattern.indices(
        nucleo_matcher::Utf32Str::new(haystack, &mut buf),
        &mut matcher,
        &mut indices,
    )?;
    indices.sort_unstable();
    indices.dedup();
    Some((score, indices))
}

/// Fuzzy-filters a slice of repository paths using a single reused `nucleo`
/// matcher, returning up to `limit` best matches as `(path, matched_char_indices)`.
pub fn fuzzy_filter_paths(
    paths: &[String],
    needle: &str,
    limit: usize,
) -> Vec<(String, Vec<u32>)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return paths
            .iter()
            .take(limit)
            .map(|p| (p.clone(), Vec::new()))
            .collect();
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths());
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        needle,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut scored: Vec<(u32, usize, Vec<u32>)> = Vec::new();
    for (idx, path) in paths.iter().enumerate() {
        buf.clear();
        indices.clear();
        if let Some(score) = pattern.indices(
            nucleo_matcher::Utf32Str::new(path, &mut buf),
            &mut matcher,
            &mut indices,
        ) {
            indices.sort_unstable();
            indices.dedup();
            scored.push((score, idx, indices.clone()));
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| paths[a.1].len().cmp(&paths[b.1].len()))
            .then_with(|| paths[a.1].cmp(&paths[b.1]))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, idx, matched)| (paths[idx].clone(), matched))
        .collect()
}

/// Branches considered "protected" (merge suggestions never offer deleting them).
pub fn is_protected_branch(name: &str) -> bool {
    matches!(
        name,
        "main" | "master" | "develop" | "development" | "trunk" | "release" | "production"
    ) || name.starts_with("release/")
}

/// Returns `true` if `branch` matches any non-empty glob pattern in `patterns`.
pub fn matches_protected_branch_patterns(branch: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|pat| crate::glob_match(pat, branch))
}
