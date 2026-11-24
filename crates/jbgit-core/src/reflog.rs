//! Reflog query types/parsers and `git cherry` patch-equivalence parser.

use crate::Oid;

/// Cache/request key for asynchronous `git cherry <head_oid> <branch>` queries
/// powering the Already Cherry-Picked highlighter (`CherryPickedCommitsHighlighter`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CherryPickedKey {
    /// Current HEAD commit OID (upstream argument to `git cherry`).
    pub head_oid: Oid,
    /// Branch name or refspec being compared against `head_oid`.
    pub branch: String,
}

/// Parses stdout of `git cherry <upstream> <head>`.
///
/// Lines prefixed with `"- "` mark commits whose patch-id already exists in
/// `<upstream>` (`Already Cherry-Picked`). Lines prefixed with `"+ "` are
/// unpicked commits and are ignored.
pub fn parse_git_cherry_output(output: &str) -> Vec<Oid> {
    let mut picked = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- ") {
            let token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(oid) = token.parse::<Oid>() {
                picked.push(oid);
            }
        }
    }
    picked.sort_unstable();
    picked.dedup();
    picked
}

/// Default page size for `git reflog show`.
pub const DEFAULT_REFLOG_LIMIT: usize = 200;

/// Request key for `git reflog show`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReflogKey {
    /// Ref to inspect (e.g. `"HEAD"` or `"main"`).
    pub ref_name: String,
    /// Maximum number of entries to return.
    pub limit: usize,
}

impl ReflogKey {
    pub fn head() -> Self {
        Self {
            ref_name: "HEAD".to_string(),
            limit: DEFAULT_REFLOG_LIMIT,
        }
    }

    pub fn for_ref(ref_name: impl Into<String>) -> Self {
        Self {
            ref_name: ref_name.into(),
            limit: DEFAULT_REFLOG_LIMIT,
        }
    }
}

/// Categorized action type extracted from a reflog message (`%gs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflogActionKind {
    Commit,
    Rebase,
    Reset,
    Checkout,
    Merge,
    CherryPick,
    Pull,
    Branch,
    Stash,
    Other,
}

impl ReflogActionKind {
    /// Classifies a git reflog subject line (`%gs`), e.g. `"commit (amend): ..."`,
    /// `"reset: moving to HEAD~1"`, `"rebase (finish): ..."`.
    pub fn classify(message: &str) -> Self {
        let prefix = message
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if prefix.starts_with("commit") {
            Self::Commit
        } else if prefix.starts_with("rebase") {
            Self::Rebase
        } else if prefix.starts_with("reset") {
            Self::Reset
        } else if prefix.starts_with("checkout") {
            Self::Checkout
        } else if prefix.starts_with("merge") {
            Self::Merge
        } else if prefix.starts_with("cherry-pick") {
            Self::CherryPick
        } else if prefix.starts_with("pull") {
            Self::Pull
        } else if prefix.starts_with("branch") || prefix.starts_with("clone") {
            Self::Branch
        } else if prefix.starts_with("stash") {
            Self::Stash
        } else {
            Self::Other
        }
    }

    pub const fn badge(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Rebase => "rebase",
            Self::Reset => "reset",
            Self::Checkout => "checkout",
            Self::Merge => "merge",
            Self::CherryPick => "pick",
            Self::Pull => "pull",
            Self::Branch => "branch",
            Self::Stash => "stash",
            Self::Other => "other",
        }
    }
}

/// Single entry returned by `git reflog show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    pub oid: Oid,
    /// Reflog selector such as `HEAD@{0}` or `main@{3}`.
    pub selector: String,
    pub action: ReflogActionKind,
    /// Full reflog message (`%gs`).
    pub message: String,
    /// Unix timestamp in seconds (`%ct`).
    pub timestamp: i64,
    /// Author name (`%an`).
    pub author: String,
}

/// Paginated result of a `git reflog show` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogBundle {
    pub key: ReflogKey,
    pub entries: Vec<ReflogEntry>,
    /// True when more entries exist beyond `key.limit`.
    pub has_more: bool,
}

/// Format string passed to `git reflog show --format=...`.
/// Records are separated by `\x1e` (RS) and fields by `\x1f` (US).
pub const REFLOG_GIT_FORMAT: &str = "%x1e%H%x1f%gd%x1f%gs%x1f%ct%x1f%an";

/// Parses stdout produced by `git reflog show --format=<REFLOG_GIT_FORMAT> -n <limit+1> <ref>`.
pub fn parse_reflog_output(raw: &str, key: ReflogKey) -> ReflogBundle {
    let mut entries = Vec::new();
    let mut has_more = false;

    for record in raw.split('\x1e') {
        let rec = record.trim_matches(['\r', '\n']);
        if rec.is_empty() {
            continue;
        }
        let mut fields = rec.splitn(5, '\x1f');
        let Some(hash) = fields.next().map(str::trim) else {
            continue;
        };
        let Ok(oid) = hash.parse::<Oid>() else {
            continue;
        };
        let selector = fields.next().unwrap_or("").trim().to_string();
        let message = fields.next().unwrap_or("").trim().to_string();
        let timestamp = fields
            .next()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let author = fields.next().unwrap_or("").trim().to_string();

        if entries.len() >= key.limit {
            has_more = true;
            break;
        }
        let action = ReflogActionKind::classify(&message);
        entries.push(ReflogEntry {
            oid,
            selector,
            action,
            message,
            timestamp,
            author,
        });
    }

    ReflogBundle {
        key,
        entries,
        has_more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_oid(byte: u8) -> (String, Oid) {
        let oid = Oid::from_bytes([byte; 20]);
        (oid.to_string(), oid)
    }

    #[test]
    fn parses_git_cherry_minus_and_ignores_plus() {
        let (h1, o1) = hex_oid(0x11);
        let (h2, _o2) = hex_oid(0x22);
        let (h3, o3) = hex_oid(0x33);
        let raw = format!(
            "+ {h2}\n- {h3} already picked subject\n- {h1}\n  - {h1}\nmalformed line\n"
        );
        let parsed = parse_git_cherry_output(&raw);
        assert_eq!(parsed, vec![o1, o3]);
    }

    #[test]
    fn parses_reflog_entries_action_kinds_and_pagination() {
        let (h1, o1) = hex_oid(0xaa);
        let (h2, o2) = hex_oid(0xbb);
        let (h3, _o3) = hex_oid(0xcc);
        let raw = format!(
            "\x1e{h1}\x1fHEAD@{{0}}\x1freset: moving to HEAD~1\x1f1700000300\x1fAlice\n\
             \x1e{h2}\x1fHEAD@{{1}}\x1fcommit (amend): fix bug\x1f1700000200\x1fBob\n\
             \x1e{h3}\x1fHEAD@{{2}}\x1frebase (finish): returning to refs/heads/main\x1f1700000100\x1fCarol\n"
        );
        let bundle = parse_reflog_output(
            &raw,
            ReflogKey {
                ref_name: "HEAD".into(),
                limit: 2,
            },
        );
        assert!(bundle.has_more);
        assert_eq!(bundle.entries.len(), 2);
        assert_eq!(bundle.entries[0].oid, o1);
        assert_eq!(bundle.entries[0].selector, "HEAD@{0}");
        assert_eq!(bundle.entries[0].action, ReflogActionKind::Reset);
        assert_eq!(bundle.entries[0].author, "Alice");
        assert_eq!(bundle.entries[1].oid, o2);
        assert_eq!(bundle.entries[1].action, ReflogActionKind::Commit);
        assert_eq!(bundle.entries[1].message, "commit (amend): fix bug");
    }

    #[test]
    fn classifies_all_common_reflog_actions() {
        assert_eq!(
            ReflogActionKind::classify("commit (initial): init"),
            ReflogActionKind::Commit
        );
        assert_eq!(
            ReflogActionKind::classify("rebase -i (pick): feat"),
            ReflogActionKind::Rebase
        );
        assert_eq!(
            ReflogActionKind::classify("reset: moving to abc1234"),
            ReflogActionKind::Reset
        );
        assert_eq!(
            ReflogActionKind::classify("checkout: moving from a to b"),
            ReflogActionKind::Checkout
        );
        assert_eq!(
            ReflogActionKind::classify("merge feat: Fast-forward"),
            ReflogActionKind::Merge
        );
        assert_eq!(
            ReflogActionKind::classify("cherry-pick: apply hotfix"),
            ReflogActionKind::CherryPick
        );
        assert_eq!(
            ReflogActionKind::classify("pull origin main: Fast-forward"),
            ReflogActionKind::Pull
        );
        assert_eq!(
            ReflogActionKind::classify("branch: Created from HEAD"),
            ReflogActionKind::Branch
        );
        assert_eq!(
            ReflogActionKind::classify("WIP on main: abc"),
            ReflogActionKind::Other
        );
    }
}
