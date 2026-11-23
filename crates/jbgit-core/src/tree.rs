//! Browse Repository at Revision (`GitBrowseRepoAtRevisionAction`): lazily
//! loaded commit trees and size-guarded blob previews.

use crate::Oid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreeEntryKind {
    Blob,
    Tree,
    /// Gitlink (`160000`): a submodule commit, shown read-only.
    Submodule,
}

/// One entry of a directory at a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    /// Repository-relative path (`/`-separated).
    pub path: String,
    pub kind: TreeEntryKind,
    /// Git file mode (`100644`, `100755`, `120000`, `040000`, `160000`).
    pub mode: u32,
    /// Blob size in bytes (blobs only).
    pub size: Option<u64>,
    /// Object id (the submodule commit for gitlinks).
    pub oid: Oid,
}

impl TreeEntry {
    pub fn is_symlink(&self) -> bool {
        self.mode == 0o120_000
    }

    pub fn is_dir(&self) -> bool {
        self.kind == TreeEntryKind::Tree
    }
}

/// One directory listing request: `dir` (`""` = root) of commit `oid`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreeKey {
    pub oid: Oid,
    pub dir: String,
}

/// Directories first, then files, each sorted by name (IDEA order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeListing {
    pub key: TreeKey,
    pub entries: Vec<TreeEntry>,
}

/// File preview request: `path` at commit `oid`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreeBlobKey {
    pub oid: Oid,
    pub path: String,
}

/// Content of a file at a revision, guarded like diff previews
/// ([`crate::MAX_DIFF_FILE_BYTES`], binary sniffing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeBlobContent {
    Text(String),
    Binary,
    TooLarge { limit: usize },
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBlob {
    pub key: TreeBlobKey,
    pub size: u64,
    pub content: TreeBlobContent,
}

/// Sorts a listing the way the tree browser shows it.
pub fn sort_tree_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: TreeEntryKind) -> TreeEntry {
        TreeEntry {
            name: name.into(),
            path: name.into(),
            kind,
            mode: 0o100_644,
            size: None,
            oid: Oid::from_bytes([0; 20]),
        }
    }

    #[test]
    fn directories_first_then_case_insensitive_names() {
        let mut v = vec![
            entry("b.txt", TreeEntryKind::Blob),
            entry("src", TreeEntryKind::Tree),
            entry("A.md", TreeEntryKind::Blob),
            entry("lib", TreeEntryKind::Submodule),
            entry("Docs", TreeEntryKind::Tree),
        ];
        sort_tree_entries(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Docs", "src", "A.md", "b.txt", "lib"]);
    }
}
