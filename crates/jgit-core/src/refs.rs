//! Reference presentation logic for the commit table: label grouping
//! (`GitRefManager.groupForTable`), ordering (`GitLabelComparator`), long-name
//! shortening (`LabelPainter.shortenRefName`) and stable branch color ids
//! (`GraphColorManagerImpl`). Pure and UI-agnostic.

use std::cmp::Ordering;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{Oid, RefInfo, RefKind};

/// `LabelPainter.MAX_LENGTH`: names longer than this are eligible for `../` folding.
pub const MAX_REF_LABEL_LEN: usize = 22;
/// Minimum readable columns reserved for the commit subject after the pill bar.
pub const MIN_SUBJECT_COLS: usize = 24;
/// Color id reserved for `main` / `master` (palette slot 0).
pub const MAIN_BRANCH_COLOR_ID: BranchColorId = BranchColorId(0);
/// Remote prefixes stripped before hashing so `origin/x` and `x` share a color.
pub const DEFAULT_REMOTE_PREFIXES: &[&str] = &["origin", "upstream"];

const TRACKED_SEPARATOR: &str = " & ";
const TWO_DOTS: &str = "..";
const ELLIPSIS: char = '…';

// ---------------------------------------------------------------------------
// Stable branch color ids
// ---------------------------------------------------------------------------

/// Deterministic color identifier for a branch or unnamed graph fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BranchColorId(pub i32);

impl BranchColorId {
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Java `String.hashCode()` over UTF-16 code units (`h = 31 * h + c`, wrapping).
pub fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, unit| h.wrapping_mul(31).wrapping_add(unit as i32))
}

/// Strips `refs/heads/`, `refs/remotes/` and one known remote prefix.
pub fn strip_remote_prefix<'a>(name: &'a str, remotes: &[&str]) -> &'a str {
    let name = name
        .strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/remotes/"))
        .unwrap_or(name);
    remotes
        .iter()
        .find_map(|remote| {
            name.strip_prefix(remote)
                .and_then(|rest| rest.strip_prefix('/'))
                .filter(|rest| !rest.is_empty())
        })
        .unwrap_or(name)
}

pub fn is_main_branch_name(name: &str) -> bool {
    matches!(name, "main" | "master")
}

/// Deterministic graph color id for a named branch fragment, with
/// [`DEFAULT_REMOTE_PREFIXES`] stripped. `main` / `master` pin to
/// [`MAIN_BRANCH_COLOR_ID`].
pub fn branch_color_id(ref_name: &str) -> BranchColorId {
    branch_color_id_with_remotes(ref_name, DEFAULT_REMOTE_PREFIXES)
}

pub fn branch_color_id_with_remotes(ref_name: &str, remotes: &[&str]) -> BranchColorId {
    let short = strip_remote_prefix(ref_name, remotes);
    if is_main_branch_name(short) {
        MAIN_BRANCH_COLOR_ID
    } else {
        BranchColorId(java_string_hash(short))
    }
}

// ---------------------------------------------------------------------------
// Ref grouping for the commit table
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefGroupKind {
    /// HEAD points here while not on a branch (always the leftmost pill).
    DetachedHead,
    LocalBranch,
    RemoteBranch,
    /// Local branch and its tracked remote branch at the same commit: `origin & x`.
    TrackedPair,
    Tag,
}

/// Whether a pill shows its text or collapses to its icon only
/// (compact view / hidden tag names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelDisplay {
    Full,
    IconOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefGroup {
    pub kind: RefGroupKind,
    /// Display text before width shortening (`main`, `origin & main`, `HEAD`).
    pub label: String,
    /// The gold HEAD marker is merged into this pill (current branch).
    pub with_head: bool,
    /// Member ref short names; the first one is the actionable primary ref.
    pub refs: Vec<String>,
    pub display: LabelDisplay,
}

impl RefGroup {
    pub fn primary_ref(&self) -> &str {
        self.refs.first().map(String::as_str).unwrap_or(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupOptions {
    /// Only the first group keeps its text; the rest collapse to icons.
    pub compact: bool,
    /// When false, tags render as icons only.
    pub show_tag_names: bool,
}

impl Default for GroupOptions {
    fn default() -> Self {
        Self {
            compact: false,
            show_tag_names: true,
        }
    }
}

impl RefInfo {
    /// Synthetic `HEAD` entry for the commit HEAD points at, so callers can
    /// feed it into [`group_refs_for_table`].
    pub fn head_marker(target_oid: Oid) -> Self {
        Self {
            name: "HEAD".to_string(),
            full_name: "HEAD".to_string(),
            target_oid,
            kind: RefKind::Head,
            upstream: None,
            ahead_behind: None,
            is_head: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RefRank {
    DetachedHead,
    CurrentBranch,
    MainBranch,
    OriginMainBranch,
    TrackedPair,
    LocalBranch,
    RemoteBranch,
    Tag,
}

/// Groups the refs pointing at a single commit into ordered pills.
///
/// - A [`RefKind::Head`] entry marks HEAD at this commit. If a local branch
///   with `is_head` is present, HEAD merges into that branch's pill;
///   otherwise it becomes a leading [`RefGroupKind::DetachedHead`] pill.
/// - A local branch and its upstream (or `<remote>/<name>`) at the same
///   commit fuse into one [`RefGroupKind::TrackedPair`].
/// - Order: detached HEAD, current branch, `main`/`master`, `origin/main`,
///   tracked pairs, local, remote, tags; ties use natural name order.
pub fn group_refs_for_table<'a, I>(refs: I, opts: GroupOptions) -> Vec<RefGroup>
where
    I: IntoIterator<Item = &'a RefInfo>,
{
    let mut head_marker = false;
    let mut locals: Vec<&RefInfo> = Vec::new();
    let mut remotes: Vec<&RefInfo> = Vec::new();
    let mut tags: Vec<&RefInfo> = Vec::new();
    for r in refs {
        match r.kind {
            RefKind::Head => head_marker = true,
            RefKind::LocalBranch => locals.push(r),
            RefKind::RemoteBranch => remotes.push(r),
            RefKind::Tag => tags.push(r),
        }
    }
    locals.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    remotes.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    tags.sort_by(|a, b| natural_cmp(&a.name, &b.name));

    let mut remote_used = vec![false; remotes.len()];
    let mut ranked: Vec<(RefRank, RefGroup)> = Vec::new();
    let has_current = locals.iter().any(|l| l.is_head);

    for local in &locals {
        let tracked = find_tracked_remote(local, &remotes, &remote_used);
        let is_main = is_main_branch_name(&local.name);
        let (group, rank) = match tracked {
            Some((idx, remote_name)) => {
                remote_used[idx] = true;
                let rank = if local.is_head {
                    RefRank::CurrentBranch
                } else if is_main {
                    RefRank::MainBranch
                } else {
                    RefRank::TrackedPair
                };
                (
                    RefGroup {
                        kind: RefGroupKind::TrackedPair,
                        label: format!("{remote_name}{TRACKED_SEPARATOR}{}", local.name),
                        with_head: local.is_head,
                        refs: vec![local.name.clone(), remotes[idx].name.clone()],
                        display: LabelDisplay::Full,
                    },
                    rank,
                )
            }
            None => {
                let rank = if local.is_head {
                    RefRank::CurrentBranch
                } else if is_main {
                    RefRank::MainBranch
                } else {
                    RefRank::LocalBranch
                };
                (
                    RefGroup {
                        kind: RefGroupKind::LocalBranch,
                        label: local.name.clone(),
                        with_head: local.is_head,
                        refs: vec![local.name.clone()],
                        display: LabelDisplay::Full,
                    },
                    rank,
                )
            }
        };
        ranked.push((rank, group));
    }

    for (idx, remote) in remotes.iter().enumerate() {
        if remote_used[idx] {
            continue;
        }
        let rank = if matches!(remote.name.as_str(), "origin/main" | "origin/master") {
            RefRank::OriginMainBranch
        } else {
            RefRank::RemoteBranch
        };
        ranked.push((
            rank,
            RefGroup {
                kind: RefGroupKind::RemoteBranch,
                label: remote.name.clone(),
                with_head: false,
                refs: vec![remote.name.clone()],
                display: LabelDisplay::Full,
            },
        ));
    }

    for tag in &tags {
        ranked.push((
            RefRank::Tag,
            RefGroup {
                kind: RefGroupKind::Tag,
                label: tag.name.clone(),
                with_head: false,
                refs: vec![tag.name.clone()],
                display: if opts.show_tag_names {
                    LabelDisplay::Full
                } else {
                    LabelDisplay::IconOnly
                },
            },
        ));
    }

    if head_marker && !has_current {
        ranked.push((
            RefRank::DetachedHead,
            RefGroup {
                kind: RefGroupKind::DetachedHead,
                label: "HEAD".to_string(),
                with_head: true,
                refs: vec!["HEAD".to_string()],
                display: LabelDisplay::Full,
            },
        ));
    }

    // Stable sort: equal ranks keep the natural order established above.
    ranked.sort_by_key(|(rank, _)| *rank);
    let mut groups: Vec<RefGroup> = ranked.into_iter().map(|(_, g)| g).collect();

    if opts.compact {
        for g in groups.iter_mut().skip(1) {
            g.display = LabelDisplay::IconOnly;
        }
    }
    groups
}

/// `GitRefManager.createTrackedGroup`: prefer the configured upstream, then
/// fall back to any `<remote>/<local name>` at the same commit.
fn find_tracked_remote(
    local: &RefInfo,
    remotes: &[&RefInfo],
    used: &[bool],
) -> Option<(usize, String)> {
    let available = |idx: &usize| !used[*idx];
    if let Some(upstream) = local.upstream.as_deref()
        && let Some(idx) = (0..remotes.len())
            .filter(available)
            .find(|&i| remotes[i].name == upstream)
    {
        let remote = upstream.split('/').next().unwrap_or(upstream);
        return Some((idx, remote.to_string()));
    }
    let suffix = format!("/{}", local.name);
    (0..remotes.len()).filter(available).find_map(|i| {
        let remote = remotes[i].name.strip_suffix(&suffix)?;
        (!remote.is_empty() && !remote.contains('/')).then(|| (i, remote.to_string()))
    })
}

/// Case-insensitive natural ordering (`feat2 < feat10`), falling back to a
/// byte-wise comparison for a total order.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let na = take_digits(&mut ai);
                let nb = take_digits(&mut bi);
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                let ord = ca.to_lowercase().cmp(cb.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

// ---------------------------------------------------------------------------
// Width-aware shortening (LabelPainter.shortenRefName)
// ---------------------------------------------------------------------------

/// Shortens `name` to fit `available_cols` terminal columns.
///
/// Names that are wider than `available_cols` *and* longer than `max_len`
/// first fold the prefix before the first `/` into `..`
/// (`feature/user-auth` → `../user-auth`); anything still too wide is
/// truncated with a trailing `…`. Widths use `unicode-width`, so CJK and
/// emoji are measured in real terminal columns.
pub fn shorten_ref_name(name: &str, max_len: usize, available_cols: usize) -> String {
    if name.width() <= available_cols {
        return name.to_string();
    }
    let mut text = name.to_string();
    if name.width() > max_len
        && let Some(sep) = name.find('/')
        && sep > TWO_DOTS.len()
        && !name[..sep].contains(' ')
    {
        text = format!("{TWO_DOTS}{}", &name[sep..]);
    }
    truncate_with_ellipsis(&text, available_cols)
}

/// Shortens a [`RefGroup`] label while preserving the `"<remote> & "` prefix
/// of a [`RefGroupKind::TrackedPair`] when folding a long branch prefix.
pub fn shorten_group_label(group: &RefGroup, max_len: usize, available_cols: usize) -> String {
    if group.label.width() <= available_cols {
        return group.label.clone();
    }
    if group.kind == RefGroupKind::TrackedPair
        && let Some(branch) = group.refs.first()
        && let Some(prefix) = group.label.strip_suffix(branch.as_str())
    {
        let prefix_w = prefix.width();
        if available_cols > prefix_w {
            let short_branch = shorten_ref_name(branch, max_len, available_cols - prefix_w);
            return format!("{prefix}{short_branch}");
        }
    }
    shorten_ref_name(&group.label, max_len, available_cols)
}

/// Truncates to at most `max_cols` columns, appending `…` when cut.
pub fn truncate_with_ellipsis(s: &str, max_cols: usize) -> String {
    if s.width() <= max_cols {
        return s.to_string();
    }
    if max_cols == 0 {
        return String::new();
    }
    let budget = max_cols - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes([n; 20])
    }

    fn local(name: &str, upstream: Option<&str>, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/heads/{name}"),
            target_oid: oid(1),
            kind: RefKind::LocalBranch,
            upstream: upstream.map(ToString::to_string),
            ahead_behind: None,
            is_head,
        }
    }

    fn remote(name: &str) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/remotes/{name}"),
            target_oid: oid(1),
            kind: RefKind::RemoteBranch,
            upstream: None,
            ahead_behind: None,
            is_head: false,
        }
    }

    fn tag(name: &str) -> RefInfo {
        RefInfo {
            name: name.to_string(),
            full_name: format!("refs/tags/{name}"),
            target_oid: oid(1),
            kind: RefKind::Tag,
            upstream: None,
            ahead_behind: None,
            is_head: false,
        }
    }

    fn labels(groups: &[RefGroup]) -> Vec<(RefGroupKind, &str, bool)> {
        groups
            .iter()
            .map(|g| (g.kind, g.label.as_str(), g.with_head))
            .collect()
    }

    #[test]
    fn java_hash_matches_jvm() {
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("main"), 3_343_801);
        assert_eq!(java_string_hash("hello"), 99_162_322);
        // Overflowing input wraps exactly like the JVM.
        assert_eq!(java_string_hash("polygenelubricants"), i32::MIN);
    }

    #[test]
    fn remote_and_local_share_color_id() {
        assert_eq!(branch_color_id("main"), MAIN_BRANCH_COLOR_ID);
        assert_eq!(branch_color_id("master"), MAIN_BRANCH_COLOR_ID);
        assert_eq!(branch_color_id("origin/main"), MAIN_BRANCH_COLOR_ID);
        assert_eq!(
            branch_color_id("refs/remotes/origin/master"),
            MAIN_BRANCH_COLOR_ID
        );
        assert_eq!(
            branch_color_id("feature/auth"),
            branch_color_id("origin/feature/auth")
        );
        assert_eq!(
            branch_color_id("feature/auth"),
            branch_color_id("refs/heads/feature/auth")
        );
        assert_ne!(
            branch_color_id("feature/auth"),
            branch_color_id("feature/other")
        );
    }

    #[test]
    fn head_main_origin_main_tag_collapse_to_two_pills() {
        let refs = [
            RefInfo::head_marker(oid(1)),
            local("main", Some("origin/main"), true),
            remote("origin/main"),
            tag("v1.0.0"),
        ];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        assert_eq!(
            labels(&groups),
            vec![
                (RefGroupKind::TrackedPair, "origin & main", true),
                (RefGroupKind::Tag, "v1.0.0", false),
            ]
        );
        assert_eq!(groups[0].refs, vec!["main", "origin/main"]);
        assert_eq!(groups[0].primary_ref(), "main");
    }

    #[test]
    fn detached_head_is_leftmost_warning_pill() {
        let refs = [
            tag("v2.0"),
            local("feature/x", None, false),
            RefInfo::head_marker(oid(1)),
        ];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        assert_eq!(
            labels(&groups),
            vec![
                (RefGroupKind::DetachedHead, "HEAD", true),
                (RefGroupKind::LocalBranch, "feature/x", false),
                (RefGroupKind::Tag, "v2.0", false),
            ]
        );
    }

    #[test]
    fn label_comparator_order() {
        let refs = [
            tag("v1"),
            remote("upstream/zeta"),
            local("feat10", None, false),
            local("feat2", None, false),
            remote("origin/main"),
            local("topic", Some("origin/topic"), false),
            remote("origin/topic"),
            local("develop", None, true),
            local("master", None, false),
        ];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        assert_eq!(
            labels(&groups),
            vec![
                (RefGroupKind::LocalBranch, "develop", true),
                (RefGroupKind::LocalBranch, "master", false),
                (RefGroupKind::RemoteBranch, "origin/main", false),
                (RefGroupKind::TrackedPair, "origin & topic", false),
                (RefGroupKind::LocalBranch, "feat2", false),
                (RefGroupKind::LocalBranch, "feat10", false),
                (RefGroupKind::RemoteBranch, "upstream/zeta", false),
                (RefGroupKind::Tag, "v1", false),
            ]
        );
    }

    #[test]
    fn tracked_pair_falls_back_to_remote_name_match() {
        let refs = [local("feat/dag", None, false), remote("fork/feat/dag")];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        assert_eq!(
            labels(&groups),
            vec![(RefGroupKind::TrackedPair, "fork & feat/dag", false)]
        );
    }

    #[test]
    fn compact_and_hidden_tags_collapse_to_icons() {
        let refs = [
            local("main", None, true),
            local("dev", None, false),
            tag("v1"),
        ];
        let compact = group_refs_for_table(
            &refs,
            GroupOptions {
                compact: true,
                show_tag_names: true,
            },
        );
        let displays: Vec<_> = compact.iter().map(|g| g.display).collect();
        assert_eq!(
            displays,
            vec![
                LabelDisplay::Full,
                LabelDisplay::IconOnly,
                LabelDisplay::IconOnly
            ]
        );

        let no_tags = group_refs_for_table(
            &refs,
            GroupOptions {
                compact: false,
                show_tag_names: false,
            },
        );
        assert_eq!(no_tags[2].kind, RefGroupKind::Tag);
        assert_eq!(no_tags[2].display, LabelDisplay::IconOnly);
        assert_eq!(no_tags[1].display, LabelDisplay::Full);
    }

    #[test]
    fn shorten_folds_prefix_then_ellipsizes() {
        let name = "feature/user-authentication-oauth2";
        assert_eq!(shorten_ref_name(name, 22, 80), name);
        assert_eq!(
            shorten_ref_name(name, 22, 29),
            "../user-authentication-oauth2"
        );
        assert_eq!(shorten_ref_name(name, 22, 20), "../user-authenticat…");
        // Short names are never folded, only truncated.
        assert_eq!(shorten_ref_name("feat/abc", 22, 5), "feat…");
        // Separator too close to the start (`ab/…`) is not folded.
        assert_eq!(
            shorten_ref_name("ab/cdefghijklmnopqrstuvwxyz", 22, 10),
            "ab/cdefgh…"
        );
    }

    #[test]
    fn shorten_measures_cjk_columns() {
        let name = "feat/登录优化登录优化登录优化";
        let out = shorten_ref_name(name, 22, 12);
        assert!(out.width() <= 12, "{out} is {} cols", out.width());
        assert!(out.starts_with("../"));
        assert!(out.ends_with('…'));
        // Never splits a wide glyph: 1 col budget left for a 2-col char → dropped.
        assert_eq!(truncate_with_ellipsis("登录", 2), "…");
    }

    #[test]
    fn shorten_tracked_pair_preserves_remote_prefix() {
        let refs = [
            local("feature/user-authentication-oauth2", Some("origin/feature/user-authentication-oauth2"), false),
            remote("origin/feature/user-authentication-oauth2"),
        ];
        let groups = group_refs_for_table(&refs, GroupOptions::default());
        assert_eq!(groups[0].kind, RefGroupKind::TrackedPair);
        assert_eq!(
            shorten_group_label(&groups[0], MAX_REF_LABEL_LEN, 38),
            "origin & ../user-authentication-oauth2"
        );
        assert_eq!(
            shorten_group_label(&groups[0], MAX_REF_LABEL_LEN, 28),
            "origin & ../user-authentica…"
        );
        // Very narrow budget falls back cleanly without breaking width.
        let tiny = shorten_group_label(&groups[0], MAX_REF_LABEL_LEN, 6);
        assert!(tiny.width() <= 6, "{tiny}");
    }
}
