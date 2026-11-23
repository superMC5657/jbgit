use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::{Oid, RewritePlan, TodoEntry, is_protected_branch, natural_cmp};

/// Typed reason a commit range cannot be rewritten by interactive rebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewritePlanError {
    NoCommits,
    NotReachable,
    ContainsMergeCommits,
    ProtectedBranch(String),
}

impl RewritePlanError {
    pub fn format_en(&self, verb: &str) -> String {
        match self {
            Self::NoCommits => "No commits yet".to_string(),
            Self::NotReachable => format!("Cannot {verb}: commit is not reachable from HEAD"),
            Self::ContainsMergeCommits => {
                format!("Cannot {verb}: the range up to HEAD contains merge commits")
            }
            Self::ProtectedBranch(branch) => {
                format!("Cannot {verb}: commit is already pushed to protected branch '{branch}'")
            }
        }
    }
}

/// Ongoing Git repository workflow / state indicator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RepositoryState {
    #[default]
    Clean,
    Merge,
    Rebase {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    RebaseInteractive {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    RebaseMerge {
        progress: Option<(usize, usize)>,
        branch: Option<String>,
    },
    CherryPick,
    Revert,
    Bisect,
    Detached,
}

impl RepositoryState {
    pub fn is_ongoing(&self) -> bool {
        !matches!(self, Self::Clean | Self::Detached)
    }

    pub fn banner_label(&self) -> Option<String> {
        match self {
            Self::Clean => None,
            Self::Detached => Some("DETACHED HEAD".to_string()),
            Self::Merge => Some("MERGING".to_string()),
            Self::CherryPick => Some("CHERRY-PICKING".to_string()),
            Self::Revert => Some("REVERTING".to_string()),
            Self::Bisect => Some("BISECTING".to_string()),
            Self::Rebase { progress, branch } => {
                Some(format_rebase("REBASING", *progress, branch.as_deref()))
            }
            Self::RebaseInteractive { progress, branch } => {
                Some(format_rebase("REBASING -i", *progress, branch.as_deref()))
            }
            Self::RebaseMerge { progress, branch } => {
                Some(format_rebase("REBASING", *progress, branch.as_deref()))
            }
        }
    }
}

fn format_rebase(prefix: &str, progress: Option<(usize, usize)>, branch: Option<&str>) -> String {
    match (branch, progress) {
        (Some(b), Some((cur, total))) => format!("{prefix} {b} ({cur}/{total})"),
        (Some(b), None) => format!("{prefix} {b}"),
        (None, Some((cur, total))) => format!("{prefix} ({cur}/{total})"),
        (None, None) => prefix.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefInfo {
    pub name: String,
    pub full_name: String,
    pub target_oid: Oid,
    pub kind: RefKind,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub is_head: bool,
}

/// One configured remote (`git remote -v`).
///
/// URLs keep their original value (credentials included) so operations can
/// use them, but the [`Debug`] output and every display path go through
/// [`redact_url`].
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    /// `remote.<name>.url` (`None` when only a push URL is configured).
    pub fetch_url: Option<String>,
    /// `remote.<name>.pushurl` (`None` = pushes go to `fetch_url`).
    pub push_url: Option<String>,
}

impl RemoteInfo {
    /// Effective push URL (`pushurl`, falling back to `url`).
    pub fn effective_push_url(&self) -> Option<&str> {
        self.push_url.as_deref().or(self.fetch_url.as_deref())
    }

    /// Fetch URL with credentials masked (empty when unset).
    pub fn display_fetch_url(&self) -> String {
        self.fetch_url.as_deref().map(redact_url).unwrap_or_default()
    }

    /// Explicit push URL with credentials masked (`None` when not configured).
    pub fn display_push_url(&self) -> Option<String> {
        self.push_url.as_deref().map(redact_url)
    }
}

impl std::fmt::Debug for RemoteInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteInfo")
            .field("name", &self.name)
            .field("fetch_url", &self.fetch_url.as_deref().map(redact_url))
            .field("push_url", &self.push_url.as_deref().map(redact_url))
            .finish()
    }
}

/// Masks the password part of a URL's userinfo
/// (`https://u:secret@h/x.git` → `https://u:***@h/x.git`).
///
/// URLs without `scheme://user:password@` (scp-style `git@host:path`, plain
/// paths, `ssh://git@host`) are returned unchanged.
pub fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return url.to_string();
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return url.to_string();
    };
    if colon + 1 == userinfo.len() {
        return url.to_string();
    }
    format!(
        "{}{}:***{}",
        &url[..scheme_end + 3],
        &userinfo[..colon],
        &rest[at..]
    )
}

/// Applies [`redact_url`] to every `scheme://…` token of a free-form line
/// (console command lines, error messages).
pub fn redact_urls_in(text: &str) -> String {
    if !text.contains("://") || !text.contains('@') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.contains("://") {
            out.push_str(&redact_url(token));
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for ch in text.chars() {
        if ch.is_whitespace() || matches!(ch, '\'' | '"' | '<' | '>') {
            flush(&mut token, &mut out);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut out);
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitSummary {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    /// Committer timestamp (seconds since epoch).
    pub timestamp: i64,
    /// Author timestamp (seconds since epoch); differs from `timestamp` after rebase / cherry-pick.
    pub author_timestamp: i64,
    pub subject: String,
    /// Message body after the subject line (may be empty). Used by the text filter.
    pub body: String,
}

impl CommitSummary {
    pub fn full_message(&self) -> String {
        if self.body.trim().is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub repo_name: String,
    pub workdir: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub state: RepositoryState,
    pub is_unborn_head: bool,
    pub head_branch: Option<String>,
    pub head_oid: Option<Oid>,
    pub refs: Vec<RefInfo>,
    pub commits: Vec<CommitSummary>,
    /// `git config user.name` (drives the `@me` user filter).
    pub user_name: Option<String>,
    /// `git config user.email`.
    pub user_email: Option<String>,
    /// Recently checked-out local branches from `.git/logs/HEAD` (newest first, ≤ 5).
    pub recent_branches: Vec<String>,
    /// `stash@{n}` entries (index 0 = newest).
    pub stashes: Vec<crate::StashEntry>,
    /// Ref name (or short SHA) a paused rebase is replaying onto.
    pub ongoing_onto: Option<String>,
    /// Configured remotes (`git remote -v`), sorted with `origin` first.
    pub remotes: Vec<RemoteInfo>,
    /// `git worktree list` (main working tree first; just the main entry when
    /// the repository has no linked working trees).
    pub worktrees: Vec<crate::WorktreeInfo>,
    /// Submodule paths relative to `workdir` (`.gitmodules`).
    pub submodules: Vec<String>,
    /// True when `.gitattributes` configures `filter=lfs`.
    pub has_lfs: bool,
    /// True when the repository is a shallow clone (`Repository::is_shallow`).
    pub is_shallow: bool,
}

impl RepoSnapshot {
    /// The working tree this snapshot was loaded from.
    pub fn current_worktree(&self) -> Option<&crate::WorktreeInfo> {
        self.worktrees.iter().find(|w| w.is_at(&self.workdir))
    }

    /// The main working tree (first `git worktree list` entry).
    pub fn main_worktree(&self) -> Option<&crate::WorktreeInfo> {
        self.worktrees.iter().find(|w| w.is_main)
    }

    /// Another working tree that has local branch `name` checked out (Checkout
    /// of `name` is refused by git while it is).
    pub fn worktree_of_branch(&self, name: &str) -> Option<&crate::WorktreeInfo> {
        self.worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(name) && !w.is_at(&self.workdir))
    }

    /// Configured remote names in snapshot order (`origin` first).
    pub fn remote_names(&self) -> impl Iterator<Item = &str> {
        self.remotes.iter().map(|r| r.name.as_str())
    }

    pub fn remote(&self, name: &str) -> Option<&RemoteInfo> {
        self.remotes.iter().find(|r| r.name == name)
    }

    /// Remote part of a remote-tracking short name (`fork/team/x` → `fork`),
    /// preferring the longest configured remote name that prefixes it.
    pub fn remote_of_tracking<'a>(&self, tracking: &'a str) -> Option<&'a str> {
        let configured = self
            .remotes
            .iter()
            .map(|r| r.name.as_str())
            .filter(|n| {
                tracking.len() > n.len()
                    && tracking.starts_with(n)
                    && tracking.as_bytes()[n.len()] == b'/'
            })
            .max_by_key(|n| n.len());
        match configured {
            Some(n) => Some(&tracking[..n.len()]),
            None => tracking.split_once('/').map(|(r, _)| r),
        }
    }

    /// Remote-tracking refs (`refs/remotes/<name>/*`) that belong to `remote`.
    pub fn tracking_refs_of(&self, remote: &str) -> Vec<&RefInfo> {
        self.refs
            .iter()
            .filter(|r| {
                r.kind == RefKind::RemoteBranch && self.remote_of_tracking(&r.name) == Some(remote)
            })
            .collect()
    }

    /// Remote used when none is given explicitly: the current branch's
    /// upstream remote → `origin` → the first configured remote.
    pub fn default_remote(&self) -> Option<String> {
        if let Some(up) = self.head_ref().and_then(|r| r.upstream.as_deref())
            && let Some(remote) = self.remote_of_tracking(up)
            && (self.remotes.is_empty() || self.remote(remote).is_some())
        {
            return Some(remote.to_string());
        }
        if self.remote("origin").is_some() {
            return Some("origin".to_string());
        }
        self.remotes
            .first()
            .map(|r| r.name.clone())
            .or_else(|| self.available_remotes().into_iter().next())
    }

    /// Returns deduplicated remote names (`origin` first, then natural order)
    /// combining configured [`Self::remotes`] with prefixes inferred from
    /// remote-tracking refs and branch upstreams.
    pub fn available_remotes(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut push_unique = |name: &str| {
            let trimmed = name.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                out.push(trimmed.to_string());
            }
        };
        for remote in &self.remotes {
            push_unique(&remote.name);
        }
        for r in &self.refs {
            if r.kind == RefKind::RemoteBranch
                && let Some((rm, _)) = r.name.split_once('/')
            {
                push_unique(rm);
            }
            if r.kind == RefKind::LocalBranch
                && let Some(up) = r.upstream.as_deref()
                && let Some((rm, _)) = up.split_once('/')
            {
                push_unique(rm);
            }
        }
        out.sort_by(|a, b| match (a == "origin", b == "origin") {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => natural_cmp(a, b),
        });
        out
    }

    pub fn local_branch(&self, name: &str) -> Option<&RefInfo> {
        self.refs
            .iter()
            .find(|r| r.kind == RefKind::LocalBranch && r.name == name)
    }

    pub fn head_ref(&self) -> Option<&RefInfo> {
        self.head_branch
            .as_deref()
            .and_then(|b| self.local_branch(b))
    }

    pub fn local_branch_names(&self) -> Vec<&str> {
        self.refs
            .iter()
            .filter(|r| r.kind == RefKind::LocalBranch)
            .map(|r| r.name.as_str())
            .collect()
    }

    pub fn tag_names(&self) -> Vec<&str> {
        self.refs
            .iter()
            .filter(|r| r.kind == RefKind::Tag)
            .map(|r| r.name.as_str())
            .collect()
    }

    pub fn commit(&self, oid: &Oid) -> Option<&CommitSummary> {
        self.commits.iter().find(|c| c.oid == *oid)
    }

    fn walk_ancestors(&self, start: Oid, target: Option<Oid>) -> (HashSet<Oid>, bool) {
        let idx: HashMap<Oid, &CommitSummary> =
            self.commits.iter().map(|c| (c.oid, c)).collect();
        let mut seen = HashSet::new();
        let mut stack = vec![start];
        while let Some(o) = stack.pop() {
            if target == Some(o) {
                return (seen, true);
            }
            if !seen.insert(o) {
                continue;
            }
            if let Some(c) = idx.get(&o) {
                stack.extend(c.parents.iter().copied());
            }
        }
        (seen, false)
    }

    /// All commit Oids reachable from `start` within the loaded snapshot.
    pub fn reachable_from(&self, start: Oid) -> HashSet<Oid> {
        self.walk_ancestors(start, None).0
    }

    /// True if `target` is reachable from `from` within the loaded snapshot.
    pub fn reaches(&self, from: Oid, target: Oid) -> bool {
        self.walk_ancestors(from, Some(target)).1
    }

    /// Commits reachable from `from` but not from `exclude` (`exclude..from`)
    /// within the loaded snapshot.
    pub fn count_exclusive(&self, from: Oid, exclude: Oid) -> usize {
        let a = self.reachable_from(from);
        let b = self.reachable_from(exclude);
        a.difference(&b).count()
    }

    /// Typed error version of [`Self::plan_rewrite`] for localized UI messages.
    pub fn plan_rewrite_typed(&self, oldest: Oid) -> Result<RewritePlan, RewritePlanError> {
        let head = self.head_oid.ok_or(RewritePlanError::NoCommits)?;
        let idx: HashMap<Oid, &CommitSummary> =
            self.commits.iter().map(|c| (c.oid, c)).collect();
        let mut chain: Vec<&CommitSummary> = Vec::new();
        let mut cur = head;
        loop {
            let c = idx.get(&cur).ok_or(RewritePlanError::NotReachable)?;
            if c.parents.len() > 1 {
                return Err(RewritePlanError::ContainsMergeCommits);
            }
            chain.push(c);
            if cur == oldest {
                break;
            }
            cur = *c
                .parents
                .first()
                .ok_or(RewritePlanError::NotReachable)?;
            if chain.len() > 5000 {
                return Err(RewritePlanError::NotReachable);
            }
        }
        if let Some(r) = self.refs.iter().find(|r| {
            r.kind == RefKind::RemoteBranch
                && r.name
                    .split_once('/')
                    .is_some_and(|(_, b)| is_protected_branch(b))
                && self.reaches(r.target_oid, oldest)
        }) {
            return Err(RewritePlanError::ProtectedBranch(r.name.clone()));
        }
        chain.reverse();
        Ok(RewritePlan {
            base: chain[0].parents.first().copied(),
            entries: chain
                .iter()
                .map(|c| TodoEntry::pick(c.oid, c.subject.clone()))
                .collect(),
            messages: chain.iter().map(|c| c.full_message()).collect(),
        })
    }

    /// Plans an interactive-rebase rewrite of `oldest..HEAD` along the
    /// first-parent chain, rejecting merge commits and commits already pushed
    /// to protected remote branches.
    pub fn plan_rewrite(&self, oldest: Oid, verb: &str) -> Result<RewritePlan, String> {
        self.plan_rewrite_typed(oldest)
            .map_err(|err| err.format_en(verb))
    }

    /// Resolves `HEAD`, ref names, unique hex prefixes (≥ 4 chars) and
    /// `~N` / `^N` suffix chains (`HEAD~3`, `main^2`, `a1b2c3^^`) against the
    /// loaded commits (`GoToHashOrRef`).
    pub fn resolve_revision(&self, expr: &str) -> Option<Oid> {
        let expr = expr.trim();
        if expr.is_empty() {
            return None;
        }
        let split = expr.find(['~', '^']).unwrap_or(expr.len());
        let (base, mut ops) = expr.split_at(split);

        let mut cur = self.resolve_base(base)?;
        let parents_of = |oid: Oid| -> Option<&[Oid]> {
            self.commits
                .iter()
                .find(|c| c.oid == oid)
                .map(|c| c.parents.as_slice())
        };
        while let Some(op) = ops.chars().next() {
            ops = &ops[1..];
            let digits_len = ops.find(|c: char| !c.is_ascii_digit()).unwrap_or(ops.len());
            let n: Option<usize> = ops[..digits_len].parse().ok();
            ops = &ops[digits_len..];
            match op {
                '~' => {
                    for _ in 0..n.unwrap_or(1) {
                        cur = *parents_of(cur)?.first()?;
                    }
                }
                '^' => match n.unwrap_or(1) {
                    0 => {}
                    k => cur = *parents_of(cur)?.get(k - 1)?,
                },
                _ => return None,
            }
        }
        Some(cur)
    }

    fn resolve_base(&self, base: &str) -> Option<Oid> {
        if base.eq_ignore_ascii_case("HEAD") || base == "@" {
            return self.head_oid;
        }
        for kind in [RefKind::LocalBranch, RefKind::RemoteBranch, RefKind::Tag] {
            if let Some(r) = self
                .refs
                .iter()
                .find(|r| r.kind == kind && (r.name == base || r.full_name == base))
            {
                return Some(r.target_oid);
            }
        }
        if base.len() >= 4 && base.chars().all(|c| c.is_ascii_hexdigit()) {
            let mut hits = self
                .commits
                .iter()
                .filter(|c| c.oid.matches_hex_prefix(base));
            let first = hits.next()?;
            if hits.next().is_none() {
                return Some(first.oid);
            }
        }
        None
    }
}

/// Granularity of background refresh requested or triggered by FS watcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RefreshScope {
    #[default]
    None,
    RefsAndGraph,
    StageStatus,
    WorktreeStatus,
    All,
}

impl RefreshScope {
    pub fn merge(self, other: Self) -> Self {
        use RefreshScope::*;
        match (self, other) {
            (None, x) | (x, None) => x,
            (All, _) | (_, All) => All,
            (a, b) if a == b => a,
            (StageStatus, WorktreeStatus) | (WorktreeStatus, StageStatus) => StageStatus,
            _ => All,
        }
    }

    pub fn needs_graph_refresh(self) -> bool {
        matches!(self, Self::RefsAndGraph | Self::All)
    }

    pub fn needs_stage_refresh(self) -> bool {
        matches!(self, Self::StageStatus | Self::WorktreeStatus | Self::All)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_refs_hashes_and_ancestry_suffixes() {
        let oid = |n: u8| Oid::from_bytes([n; 20]);
        let c = |n: u8, parents: &[u8]| CommitSummary {
            oid: oid(n),
            parents: parents.iter().map(|&p| oid(p)).collect(),
            ..Default::default()
        };
        let snap = RepoSnapshot {
            head_oid: Some(oid(4)),
            refs: vec![RefInfo {
                name: "main".into(),
                full_name: "refs/heads/main".into(),
                target_oid: oid(4),
                kind: RefKind::LocalBranch,
                upstream: None,
                ahead_behind: None,
                is_head: true,
            }],
            commits: vec![c(4, &[3, 2]), c(3, &[1]), c(2, &[1]), c(1, &[])],
            ..Default::default()
        };
        assert_eq!(snap.resolve_revision("HEAD"), Some(oid(4)));
        assert_eq!(snap.resolve_revision("main~2"), Some(oid(1)));
        assert_eq!(snap.resolve_revision("HEAD^2"), Some(oid(2)));
        assert_eq!(snap.resolve_revision("HEAD^^"), Some(oid(1)));
        assert_eq!(snap.resolve_revision("0303"), Some(oid(3)));
        assert_eq!(snap.resolve_revision("HEAD~9"), None);
        assert_eq!(snap.resolve_revision("nope"), None);
    }

    fn remote(name: &str) -> RemoteInfo {
        RemoteInfo {
            name: name.into(),
            fetch_url: Some(format!("https://example.com/{name}.git")),
            push_url: None,
        }
    }

    fn local(name: &str, upstream: Option<&str>, is_head: bool) -> RefInfo {
        RefInfo {
            name: name.into(),
            full_name: format!("refs/heads/{name}"),
            target_oid: Oid::ZERO,
            kind: RefKind::LocalBranch,
            upstream: upstream.map(Into::into),
            ahead_behind: None,
            is_head,
        }
    }

    #[test]
    fn redacts_url_passwords_only() {
        assert_eq!(redact_url("https://u:secret@h/x.git"), "https://u:***@h/x.git");
        assert_eq!(
            redact_url("https://u:p%40ss@host:8443/a/b?x=1"),
            "https://u:***@host:8443/a/b?x=1"
        );
        assert_eq!(redact_url("https://token@h/x.git"), "https://token@h/x.git");
        assert_eq!(redact_url("ssh://git@h:22/x.git"), "ssh://git@h:22/x.git");
        assert_eq!(redact_url("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(redact_url("/srv/repos/x.git"), "/srv/repos/x.git");
        assert_eq!(redact_url("https://h/x@y.git"), "https://h/x@y.git");
        assert_eq!(
            redact_urls_in("git remote add fork https://u:pw@h/x.git"),
            "git remote add fork https://u:***@h/x.git"
        );
        assert_eq!(redact_urls_in("plain text"), "plain text");
        let info = RemoteInfo {
            name: "o".into(),
            fetch_url: Some("https://u:secret@h/x.git".into()),
            push_url: Some("https://u:other@h/y.git".into()),
        };
        let dbg = format!("{info:?}");
        assert!(!dbg.contains("secret") && !dbg.contains("other"), "{dbg}");
        assert_eq!(info.display_fetch_url(), "https://u:***@h/x.git");
        assert_eq!(info.effective_push_url(), Some("https://u:other@h/y.git"));
    }

    #[test]
    fn default_remote_prefers_upstream_then_origin_then_first() {
        let mut snap = RepoSnapshot {
            head_branch: Some("main".into()),
            remotes: vec![remote("origin"), remote("my/fork"), remote("zeta")],
            refs: vec![local("main", Some("my/fork/main"), true)],
            ..Default::default()
        };
        assert_eq!(snap.default_remote().as_deref(), Some("my/fork"));
        snap.refs = vec![local("main", None, true)];
        assert_eq!(snap.default_remote().as_deref(), Some("origin"));
        snap.remotes = vec![remote("zeta"), remote("alpha")];
        assert_eq!(snap.default_remote().as_deref(), Some("zeta"));
        snap.remotes.clear();
        assert_eq!(snap.default_remote(), None);
        assert_eq!(snap.remote_of_tracking("origin/feat/x"), Some("origin"));
    }

    #[test]
    fn available_remotes_merges_and_prioritizes_origin() {
        let snap = RepoSnapshot {
            remotes: vec![remote("zebra"), remote("origin")],
            refs: vec![
                RefInfo {
                    name: "main".into(),
                    full_name: "refs/heads/main".into(),
                    target_oid: Oid::ZERO,
                    kind: RefKind::LocalBranch,
                    upstream: Some("upstream/main".into()),
                    ahead_behind: None,
                    is_head: true,
                },
                RefInfo {
                    name: "fork/feat".into(),
                    full_name: "refs/remotes/fork/feat".into(),
                    target_oid: Oid::ZERO,
                    kind: RefKind::RemoteBranch,
                    upstream: None,
                    ahead_behind: None,
                    is_head: false,
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            snap.available_remotes(),
            vec!["origin", "fork", "upstream", "zebra"]
        );
    }
}
