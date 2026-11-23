//! Unified 4 MiB size-guarded, 8 KiB binary-sniffed blob & worktree reader shared
//! between two-document [`crate::details`] diffs and three-document [`crate::stage`] diffs.

use std::fs;
use std::path::Path;

use git2::{AttrCheckFlags, ObjectType, Repository};
use jbgit_core::{MAX_DIFF_FILE_BYTES, is_binary_bytes};

/// Returns `true` when git attributes configure `filter=lfs` for `path`.
pub(crate) fn path_has_lfs_filter(repo: &Repository, path: &str) -> bool {
    repo.get_attr(
        Path::new(path),
        "filter",
        AttrCheckFlags::FILE_THEN_INDEX | AttrCheckFlags::NO_SYSTEM,
    )
    .ok()
    .flatten()
        == Some("lfs")
}

pub(crate) enum CheckedBlob {
    Missing,
    TooLarge { mode: Option<u32> },
    Loaded {
        bytes: Vec<u8>,
        mode: Option<u32>,
        is_binary: bool,
    },
}

impl CheckedBlob {
    pub(crate) fn empty() -> Self {
        Self::Loaded {
            bytes: Vec::new(),
            mode: None,
            is_binary: false,
        }
    }

    pub(crate) fn is_too_large(&self) -> bool {
        matches!(self, Self::TooLarge { .. })
    }

    pub(crate) fn exists(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    pub(crate) fn mode(&self) -> Option<u32> {
        match self {
            Self::Missing => None,
            Self::TooLarge { mode } | Self::Loaded { mode, .. } => *mode,
        }
    }

    pub(crate) fn into_parts(self) -> (Option<Vec<u8>>, Option<u32>, bool) {
        match self {
            Self::Loaded {
                bytes,
                mode,
                is_binary,
            } => (Some(bytes), mode, is_binary),
            Self::TooLarge { mode } => (None, mode, false),
            Self::Missing => (None, None, false),
        }
    }

    /// Collapses `Missing` into empty non-binary bytes for two-document diff callers,
    /// returning `None` when the payload exceeded [`MAX_DIFF_FILE_BYTES`].
    pub(crate) fn into_bytes_or_too_large(self) -> Option<(Vec<u8>, bool)> {
        match self {
            Self::TooLarge { .. } => None,
            Self::Missing => Some((Vec::new(), false)),
            Self::Loaded {
                bytes, is_binary, ..
            } => Some((bytes, is_binary)),
        }
    }
}

pub(crate) fn read_blob(repo: &Repository, oid: git2::Oid, mode: Option<u32>) -> CheckedBlob {
    if oid.is_zero() {
        return CheckedBlob::Missing;
    }
    if let Ok(odb) = repo.odb()
        && let Ok((size, ObjectType::Blob)) = odb.read_header(oid)
        && size > MAX_DIFF_FILE_BYTES
    {
        return CheckedBlob::TooLarge { mode };
    }
    match repo.find_blob(oid) {
        Ok(blob) => {
            if blob.size() > MAX_DIFF_FILE_BYTES {
                return CheckedBlob::TooLarge { mode };
            }
            let content = blob.content();
            let is_binary = is_binary_bytes(content);
            CheckedBlob::Loaded {
                bytes: content.to_vec(),
                mode,
                is_binary,
            }
        }
        Err(_) => CheckedBlob::Loaded {
            bytes: Vec::new(),
            mode,
            is_binary: false,
        },
    }
}

pub(crate) fn read_tree_path(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    rel_path: &str,
) -> CheckedBlob {
    let Ok(entry) = tree.get_path(Path::new(rel_path)) else {
        return CheckedBlob::Missing;
    };
    let mode = entry.filemode() as u32;
    if entry.kind() != Some(ObjectType::Blob) {
        return CheckedBlob::Missing;
    }
    read_blob(repo, entry.id(), Some(mode))
}

pub(crate) fn read_head_tree_path(repo: &Repository, rel_path: &str) -> CheckedBlob {
    let Ok(head) = repo.head() else {
        return CheckedBlob::Missing;
    };
    let Ok(commit) = head.peel_to_commit() else {
        return CheckedBlob::Missing;
    };
    let Ok(tree) = commit.tree() else {
        return CheckedBlob::Missing;
    };
    read_tree_path(repo, &tree, rel_path)
}

pub(crate) fn read_index_entry(repo: &Repository, rel_path: &str) -> (CheckedBlob, bool) {
    let Ok(index) = repo.index() else {
        return (CheckedBlob::Missing, false);
    };
    let path_obj = Path::new(rel_path);
    let is_conflicted = index.has_conflicts()
        && (1..=3).any(|stage| index.get_path(path_obj, stage).is_some());

    if let Some(entry) = index.get_path(path_obj, 0) {
        return (
            read_blob(repo, entry.id, Some(entry.mode)),
            is_conflicted,
        );
    }
    (CheckedBlob::Missing, is_conflicted)
}

pub(crate) fn read_worktree_path(workdir: &Path, rel_path: &str) -> CheckedBlob {
    let full = workdir.join(rel_path);
    let Ok(meta) = fs::metadata(&full) else {
        return CheckedBlob::Missing;
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(if meta.permissions().mode() & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        })
    };
    #[cfg(not(unix))]
    let mode = Some(0o100644);

    if meta.len() > MAX_DIFF_FILE_BYTES as u64 {
        return CheckedBlob::TooLarge { mode };
    }
    let Ok(bytes) = fs::read(&full) else {
        return CheckedBlob::Missing;
    };
    if bytes.len() > MAX_DIFF_FILE_BYTES {
        return CheckedBlob::TooLarge { mode };
    }
    let is_binary = is_binary_bytes(&bytes);
    CheckedBlob::Loaded {
        bytes,
        mode,
        is_binary,
    }
}
