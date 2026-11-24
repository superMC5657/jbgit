//! Working-tree model (`git worktree`, Spec 12 Issue 10): the porcelain list
//! parser (`GitWorktreeListParser`), the branch choice of `worktree add`, and
//! the git version gate (`GitWorktreeSupportStatus`).

use std::path::{Path, PathBuf};

use crate::Oid;

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// `None` for bare entries and unborn heads.
    pub head: Option<Oid>,
    /// Short branch name (`refs/heads/` stripped); `None` when detached / bare.
    pub branch: Option<String>,
    /// The main working tree (always listed first by git).
    pub is_main: bool,
    pub is_bare: bool,
    pub detached: bool,
    /// `Some(reason)` when locked (`reason` may be empty).
    pub locked: Option<String>,
    /// `Some(reason)` when `git worktree prune` would remove it.
    pub prunable: Option<String>,
}

impl WorktreeInfo {
    /// Last path component (display name of the working tree).
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .and_then(|s| s.to_str())
            .map_or_else(|| self.path.display().to_string(), ToString::to_string)
    }

    pub fn is_locked(&self) -> bool {
        self.locked.is_some()
    }

    pub fn is_prunable(&self) -> bool {
        self.prunable.is_some()
    }

    /// Whether this entry lives at `path` (case-insensitive on Windows,
    /// separator-agnostic).
    pub fn is_at(&self, path: &Path) -> bool {
        same_path(&self.path, path)
    }
}

/// Compares two filesystem paths the way git prints them (forward slashes on
/// Windows) against canonicalized `PathBuf`s.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let normalized = if let Some(rest) = s.strip_prefix("//?/") {
            if let Some(unc_rest) = rest
                .get(..4)
                .filter(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
                .map(|_| &rest[4..])
            {
                format!("//{unc_rest}")
            } else {
                rest.to_string()
            }
        } else {
            s
        };
        let trimmed = normalized.trim_end_matches('/');
        if cfg!(windows) {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        }
    };
    norm(a) == norm(b)
}

/// Branch choice of `git worktree add` (`GitCreateWorkingTreeService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeBranch {
    /// Check out an existing local branch.
    Existing(String),
    /// `-b <name> <start>`.
    New { name: String, start: String },
    /// `--detach <commit>`.
    Detached(Oid),
}

impl WorktreeBranch {
    /// Branch name the new working tree ends up on (`None` when detached).
    pub fn branch_name(&self) -> Option<&str> {
        match self {
            Self::Existing(name) | Self::New { name, .. } => Some(name),
            Self::Detached(_) => None,
        }
    }
}

/// Parses `git worktree list --porcelain` output. Records are separated by
/// an empty line (or, with `-z`, by an empty NUL-terminated field); both
/// separators are accepted so the same parser serves git < 2.36.
pub fn parse_worktree_list_porcelain(output: &[u8]) -> Vec<WorktreeInfo> {
    let text = String::from_utf8_lossy(output);
    let nul = text.contains('\0');
    let fields: Vec<&str> = if nul {
        text.split('\0').collect()
    } else {
        text.split('\n').map(|l| l.trim_end_matches('\r')).collect()
    };

    let mut out = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;
    let flush = |cur: &mut Option<WorktreeInfo>, out: &mut Vec<WorktreeInfo>| {
        if let Some(mut wt) = cur.take() {
            if wt.branch.is_none() && !wt.is_bare {
                wt.detached = true;
            }
            out.push(wt);
        }
    };
    for field in fields {
        if field.is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        match key {
            "worktree" => {
                flush(&mut cur, &mut out);
                cur = Some(WorktreeInfo {
                    path: PathBuf::from(value),
                    ..WorktreeInfo::default()
                });
            }
            _ => {
                let Some(wt) = cur.as_mut() else { continue };
                match key {
                    "HEAD" => wt.head = value.parse::<Oid>().ok().filter(|o| !o.is_zero()),
                    "branch" => {
                        wt.branch = Some(
                            value
                                .strip_prefix("refs/heads/")
                                .unwrap_or(value)
                                .to_string(),
                        );
                    }
                    "bare" => wt.is_bare = true,
                    "detached" => wt.detached = true,
                    "locked" => wt.locked = Some(value.to_string()),
                    "prunable" => wt.prunable = Some(value.to_string()),
                    _ => {}
                }
            }
        }
    }
    flush(&mut cur, &mut out);
    if let Some(first) = out.first_mut() {
        first.is_main = true;
    }
    out
}

/// `git --version` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// `git worktree remove` / `lock --reason` (the feature's minimum).
    pub const WORKTREES: Self = Self::new(2, 17, 0);
    /// `git rebase --rebase-merges` (replaced `--preserve-merges`).
    pub const REBASE_MERGES: Self = Self::new(2, 18, 0);
    /// `git worktree list --porcelain -z`.
    pub const WORKTREE_LIST_Z: Self = Self::new(2, 36, 0);
    /// `git rebase --update-refs`.
    pub const REBASE_UPDATE_REFS: Self = Self::new(2, 38, 0);

    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses `git version 2.45.1.windows.1` (or a bare `2.45.1`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix("git version ").unwrap_or(s);
        let mut nums = s
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty());
        let major = nums.next()?.parse().ok()?;
        let minor = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let patch = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Some(Self::new(major, minor, patch))
    }

    pub fn supports_worktrees(self) -> bool {
        self >= Self::WORKTREES
    }

    pub fn supports_rebase_merges(self) -> bool {
        self >= Self::REBASE_MERGES
    }

    pub fn supports_rebase_update_refs(self) -> bool {
        self >= Self::REBASE_UPDATE_REFS
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Default location of a new working tree: `<parent>/<repo>-<branch>` with
/// `/` in branch names flattened to `-` (`[worktree] default_parent` replaces
/// the repository's parent directory).
pub fn default_worktree_path(main: &Path, parent: Option<&Path>, label: &str) -> PathBuf {
    let repo = main
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("worktree");
    let label: String = label
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | ' ') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let dir = parent
        .map(Path::to_path_buf)
        .or_else(|| main.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    dir.join(format!("{repo}-{label}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn parses_main_linked_detached_locked_and_prunable() {
        let out = format!(
            "worktree /repo/main\nHEAD {A}\nbranch refs/heads/main\n\n\
             worktree /repo/wt-feature\nHEAD {B}\nbranch refs/heads/feature/x\nlocked on usb disk\n\n\
             worktree /repo/wt-detached\nHEAD {A}\ndetached\nlocked\n\n\
             worktree /tmp/gone\nHEAD {B}\ndetached\nprunable gitdir file points to non-existent location\n\n"
        );
        let list = parse_worktree_list_porcelain(out.as_bytes());
        assert_eq!(list.len(), 4);
        assert!(list[0].is_main && !list[1].is_main);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[0].head, A.parse().ok());
        assert!(!list[0].detached);
        assert_eq!(list[1].branch.as_deref(), Some("feature/x"));
        assert_eq!(list[1].locked.as_deref(), Some("on usb disk"));
        assert_eq!(list[1].name(), "wt-feature");
        assert!(list[2].detached && list[2].branch.is_none());
        assert_eq!(list[2].locked.as_deref(), Some(""));
        assert!(list[2].is_locked() && !list[2].is_prunable());
        assert_eq!(
            list[3].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );
    }

    #[test]
    fn parses_bare_main_and_nul_separated_records() {
        let out = format!(
            "worktree /srv/repo.git\0bare\0\0worktree /srv/wt\0HEAD {A}\0branch refs/heads/dev\0locked reason\nwith newline\0\0"
        );
        let list = parse_worktree_list_porcelain(out.as_bytes());
        assert_eq!(list.len(), 2);
        assert!(list[0].is_main && list[0].is_bare && !list[0].detached);
        assert!(list[0].head.is_none() && list[0].branch.is_none());
        assert_eq!(list[1].branch.as_deref(), Some("dev"));
        assert_eq!(list[1].locked.as_deref(), Some("reason\nwith newline"));
    }

    #[test]
    fn parses_crlf_and_unborn_head() {
        let out = "worktree C:/w/main\r\nHEAD 0000000000000000000000000000000000000000\r\nbranch refs/heads/main\r\n\r\n";
        let list = parse_worktree_list_porcelain(out.as_bytes());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, PathBuf::from("C:/w/main"));
        assert!(list[0].head.is_none());
        assert!(list[0].is_at(Path::new(r"C:\w\main")));
        assert!(parse_worktree_list_porcelain(b"").is_empty());
    }

    #[test]
    fn git_version_gate() {
        let v = GitVersion::parse("git version 2.45.1.windows.1").unwrap();
        assert_eq!(v, GitVersion::new(2, 45, 1));
        assert!(v.supports_worktrees());
        assert!(
            !GitVersion::parse("git version 2.16.6")
                .unwrap()
                .supports_worktrees()
        );
        assert!(GitVersion::parse("2.17").unwrap().supports_worktrees());
        assert_eq!(GitVersion::parse("garbage"), None);
        assert_eq!(GitVersion::new(2, 36, 0).to_string(), "2.36.0");
    }

    #[test]
    fn default_path_is_sibling_of_main() {
        let p = default_worktree_path(Path::new("/src/jbgit"), None, "feature/login");
        assert_eq!(p, PathBuf::from("/src/jbgit-feature-login"));
        let p = default_worktree_path(Path::new("/src/jbgit"), Some(Path::new("/wt")), "abc1234");
        assert_eq!(p, PathBuf::from("/wt/jbgit-abc1234"));
    }

    #[test]
    fn same_path_matches_windows_verbatim_unc_and_wsl_paths() {
        assert!(same_path(
            Path::new(r"\\?\UNC\wsl.localhost\Ubuntu\home\user\repo"),
            Path::new("//wsl.localhost/Ubuntu/home/user/repo"),
        ));
        assert!(same_path(
            Path::new(r"\\?\unc\server\share\proj\"),
            Path::new(r"\\server\share\proj"),
        ));
        assert!(same_path(
            Path::new(r"\\?\C:\Users\repo"),
            Path::new("C:/Users/repo/"),
        ));
        // Non-ASCII right after \\?\ must not panic on UTF-8 boundary
        assert!(same_path(
            Path::new(r"\\?\中文\repo"),
            Path::new("中文/repo"),
        ));
    }
}
